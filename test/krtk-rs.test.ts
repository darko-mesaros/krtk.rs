import * as cdk from 'aws-cdk-lib';
import { Template, Match } from 'aws-cdk-lib/assertions';
import { KrtkRsStack } from '../lib/krtk-rs-stack';
import { CertificateStack } from '../lib/certificate-stack';
import { SecretsStack } from '../lib/secrets-stack';

const TEST_ENV = { account: '123456789012', region: 'us-west-2' };

/**
 * Builds the full app graph the same way bin/krtk-rs.ts does, so the assertions
 * below exercise the real wiring (cross-region cert + secret) rather than a
 * hand-stubbed stack.
 */
function synthKrtkRsStack(): Template {
  const app = new cdk.App();

  const certStack = new CertificateStack(app, 'TestCertificateStack', {
    env: { ...TEST_ENV, region: 'us-east-1' },
    crossRegionReferences: true,
  });

  const secretsStack = new SecretsStack(app, 'TestSecretsStack', {
    env: TEST_ENV,
    crossRegionReferences: true,
  });

  const stack = new KrtkRsStack(app, 'TestKrtkRsStack', {
    env: TEST_ENV,
    certificateArn: certStack.certificate.certificateArn,
    googleApiKeySecret: secretsStack.googleApiSecret,
    crossRegionReferences: true,
  });

  return Template.fromStack(stack);
}

describe('KrtkRsStack', () => {
  let template: Template;

  beforeAll(() => {
    template = synthKrtkRsStack();
  });

  describe('DynamoDB link table', () => {
    test('creates exactly one table with LinkId as the partition key', () => {
      template.resourceCountIs('AWS::DynamoDB::GlobalTable', 1);
      template.hasResourceProperties('AWS::DynamoDB::GlobalTable', {
        KeySchema: [{ AttributeName: 'LinkId', KeyType: 'HASH' }],
      });
    });

    test('declares the TimeStampIndex GSI keyed on SortKey + TimeStamp', () => {
      template.hasResourceProperties('AWS::DynamoDB::GlobalTable', {
        GlobalSecondaryIndexes: Match.arrayWith([
          Match.objectLike({
            IndexName: 'TimeStampIndex',
            KeySchema: [
              { AttributeName: 'SortKey', KeyType: 'HASH' },
              { AttributeName: 'TimeStamp', KeyType: 'RANGE' },
            ],
            Projection: { ProjectionType: 'ALL' },
          }),
        ]),
      });
    });

    test('declares all key attributes with the expected types', () => {
      template.hasResourceProperties('AWS::DynamoDB::GlobalTable', {
        AttributeDefinitions: Match.arrayWith([
          { AttributeName: 'LinkId', AttributeType: 'S' },
          { AttributeName: 'SortKey', AttributeType: 'S' },
          { AttributeName: 'TimeStamp', AttributeType: 'N' },
        ]),
      });
    });
  });

  describe('Lambda functions', () => {
    test('creates the four application functions on provided.al2023', () => {
      // The stack also synthesizes CDK-managed helper functions (bucket
      // deployment, auto-delete-objects), so assert on the custom runtime
      // rather than a bare resourceCountIs over every function.
      const functions = template.findResources('AWS::Lambda::Function', {
        Properties: { Runtime: 'provided.al2023' },
      });
      expect(Object.keys(functions)).toHaveLength(4);
    });

    test('every application function receives TABLE_NAME and SHORTENER_DOMAIN', () => {
      const functions = template.findResources('AWS::Lambda::Function', {
        Properties: { Runtime: 'provided.al2023' },
      });

      for (const fn of Object.values(functions)) {
        const env = (fn as any).Properties.Environment.Variables;
        expect(env).toHaveProperty('TABLE_NAME');
        expect(env.SHORTENER_DOMAIN).toBe('krtk.rs');
      }
    });

    test('createLink is granted read access to the Google API key secret', () => {
      const functions = template.findResources('AWS::Lambda::Function', {
        Properties: { Runtime: 'provided.al2023' },
      });
      const withSecret = Object.values(functions).filter(
        (fn) => (fn as any).Properties.Environment.Variables.GOOGLE_API_KEY_SECRET !== undefined,
      );
      expect(withSecret).toHaveLength(1);
    });

    test('processAnalytics is wired to the Kinesis stream via an event source mapping', () => {
      template.resourceCountIs('AWS::Lambda::EventSourceMapping', 1);
      template.hasResourceProperties('AWS::Lambda::EventSourceMapping', {
        BatchSize: 1,
        StartingPosition: 'TRIM_HORIZON',
      });
    });
  });

  describe('HTTP API', () => {
    test('creates the API with a non-default prod stage that is throttled', () => {
      template.resourceCountIs('AWS::ApiGatewayV2::Api', 1);
      template.hasResourceProperties('AWS::ApiGatewayV2::Stage', {
        StageName: 'prod',
        AutoDeploy: true,
        DefaultRouteSettings: {
          ThrottlingRateLimit: 5,
          ThrottlingBurstLimit: 10,
        },
      });
    });

    test('exposes exactly the three expected routes', () => {
      const routes = template.findResources('AWS::ApiGatewayV2::Route');
      const routeKeys = Object.values(routes).map((r) => (r as any).Properties.RouteKey).sort();
      expect(routeKeys).toEqual([
        'GET /api/links',
        'GET /{linkId}',
        'POST /api/links',
      ]);
    });

    test('configures CORS preflight for the browser client', () => {
      template.hasResourceProperties('AWS::ApiGatewayV2::Api', {
        CorsConfiguration: Match.objectLike({
          AllowMethods: Match.arrayWith(['GET', 'POST', 'OPTIONS']),
          AllowHeaders: ['content-type'],
        }),
      });
    });
  });

  describe('CloudFront distribution', () => {
    test('serves the krtk.rs alias over HTTPS only', () => {
      template.resourceCountIs('AWS::CloudFront::Distribution', 1);
      template.hasResourceProperties('AWS::CloudFront::Distribution', {
        DistributionConfig: Match.objectLike({
          Aliases: ['krtk.rs'],
          DefaultCacheBehavior: Match.objectLike({
            ViewerProtocolPolicy: 'redirect-to-https',
          }),
        }),
      });
    });

    test('maps 404 responses to index.html so the SPA can route them', () => {
      template.hasResourceProperties('AWS::CloudFront::Distribution', {
        DistributionConfig: Match.objectLike({
          CustomErrorResponses: Match.arrayWith([
            Match.objectLike({
              ErrorCode: 404,
              ResponseCode: 404,
              ResponsePagePath: '/index.html',
            }),
          ]),
        }),
      });
    });

    test('carves out behaviours for the API, assets, index and the legal pages', () => {
      const distributions = template.findResources('AWS::CloudFront::Distribution');
      const config = (Object.values(distributions)[0] as any).Properties.DistributionConfig;
      const patterns = config.CacheBehaviors.map((b: any) => b.PathPattern).sort();
      expect(patterns).toEqual(['/?*', '/api/*', '/assets/*', '/index.html', '/privacy', '/terms']);
    });

    // Regression guard for a live 404 on the apex. In a CloudFront path pattern '?'
    // matches exactly one character, so '/?*' matches every root-level path with at
    // least one character. defaultRootObject rewrites '/' to '/index.html', so without
    // its own carve-out that rewritten path is served by the visit_link Lambda and 404s.
    // These assert ROUTING PRECEDENCE, which the config-shape tests above cannot see.
    test('/index.html is routed to S3, not swallowed by the /?* catch-all', () => {
      const distributions = template.findResources('AWS::CloudFront::Distribution');
      const config = (Object.values(distributions)[0] as any).Properties.DistributionConfig;

      const indexBehavior = config.CacheBehaviors.find((b: any) => b.PathPattern === '/index.html');
      expect(indexBehavior).toBeDefined();

      // The S3 origin, not the API Gateway origin.
      const s3OriginId = config.DefaultCacheBehavior.TargetOriginId;
      expect(indexBehavior.TargetOriginId).toBe(s3OriginId);
    });

    test('/index.html is matched before the /?* catch-all', () => {
      const distributions = template.findResources('AWS::CloudFront::Distribution');
      const config = (Object.values(distributions)[0] as any).Properties.DistributionConfig;
      const patterns = config.CacheBehaviors.map((b: any) => b.PathPattern);

      // CloudFront evaluates CacheBehaviors in array order.
      expect(patterns.indexOf('/index.html')).toBeLessThan(patterns.indexOf('/?*'));
    });

    test('the 404 fallback points at a concrete object, not the bare root', () => {
      // responsePagePath '/' re-enters defaultRootObject resolution and falls into
      // '/?*', so the error page fetch 404s too and the client gets an empty body.
      template.hasResourceProperties('AWS::CloudFront::Distribution', {
        DistributionConfig: Match.objectLike({
          CustomErrorResponses: Match.arrayWith([
            Match.objectLike({ ErrorCode: 404, ResponsePagePath: '/index.html' }),
          ]),
        }),
      });
    });

    test('attaches the realtime log config to the link-redirect behaviour', () => {
      template.resourceCountIs('AWS::CloudFront::RealtimeLogConfig', 1);
      const distributions = template.findResources('AWS::CloudFront::Distribution');
      const config = (Object.values(distributions)[0] as any).Properties.DistributionConfig;
      const redirectBehavior = config.CacheBehaviors.find((b: any) => b.PathPattern === '/?*');
      expect(redirectBehavior.RealtimeLogConfigArn).toBeDefined();
    });

    test('forces text/html on the extensionless /terms and /privacy objects', () => {
      template.resourceCountIs('AWS::CloudFront::ResponseHeadersPolicy', 2);
      template.hasResourceProperties('AWS::CloudFront::ResponseHeadersPolicy', {
        ResponseHeadersPolicyConfig: Match.objectLike({
          CustomHeadersConfig: {
            Items: [
              { Header: 'content-type', Value: 'text/html; charset=utf-8', Override: true },
            ],
          },
        }),
      });
    });
  });

  describe('S3 hosting bucket', () => {
    test('creates the krtk.rs bucket with versioning enabled', () => {
      template.hasResourceProperties('AWS::S3::Bucket', {
        BucketName: 'krtk.rs',
        VersioningConfiguration: { Status: 'Enabled' },
      });
    });

    test('deploys the website contents and invalidates the distribution', () => {
      template.resourceCountIs('Custom::CDKBucketDeployment', 1);
      template.hasResourceProperties('Custom::CDKBucketDeployment', {
        DistributionPaths: ['/*'],
      });
    });
  });

  // ---------------------------------------------------------------------------
  // Hardening guards. These are the regression tests that matter most: they fail
  // loudly if the bucket is ever reopened to the public internet or the link table
  // loses its protections.
  // ---------------------------------------------------------------------------
  describe('Hardening: bucket is private behind CloudFront OAC', () => {
    test('blocks all four forms of public access', () => {
      template.hasResourceProperties('AWS::S3::Bucket', {
        BucketName: 'krtk.rs',
        PublicAccessBlockConfiguration: {
          BlockPublicAcls: true,
          BlockPublicPolicy: true,
          IgnorePublicAcls: true,
          RestrictPublicBuckets: true,
        },
      });
    });

    test('never grants access to a wildcard principal in an Allow statement', () => {
      // A wildcard principal on the enforceSSL *Deny* statement is correct and expected;
      // only an Allow with a wildcard principal would reopen the bucket.
      const policies = template.findResources('AWS::S3::BucketPolicy');

      for (const policy of Object.values(policies)) {
        const statements = (policy as any).Properties.PolicyDocument.Statement;
        for (const statement of statements) {
          if (statement.Effect !== 'Allow') continue;
          expect(statement.Principal).not.toEqual('*');
          expect(statement.Principal?.AWS).not.toEqual('*');
        }
      }
    });

    test('denies any non-TLS request', () => {
      template.hasResourceProperties('AWS::S3::BucketPolicy', {
        PolicyDocument: {
          Statement: Match.arrayWith([
            Match.objectLike({
              Effect: 'Deny',
              Condition: { Bool: { 'aws:SecureTransport': 'false' } },
            }),
          ]),
        },
      });
    });

    test('scopes the CloudFront grant to this distribution only', () => {
      // Without the SourceArn condition any CloudFront distribution in any account
      // could read the bucket via OAC.
      template.hasResourceProperties('AWS::S3::BucketPolicy', {
        PolicyDocument: {
          Statement: Match.arrayWith([
            Match.objectLike({
              Action: 's3:GetObject',
              Effect: 'Allow',
              Condition: { StringEquals: { 'AWS:SourceArn': Match.anyValue() } },
            }),
          ]),
        },
      });
    });

    test('grants read only to the CloudFront service principal', () => {
      template.hasResourceProperties('AWS::S3::BucketPolicy', {
        PolicyDocument: {
          Statement: Match.arrayWith([
            Match.objectLike({
              Action: 's3:GetObject',
              Effect: 'Allow',
              Principal: { Service: 'cloudfront.amazonaws.com' },
            }),
          ]),
        },
      });
    });

    test('attaches an origin access control to the distribution', () => {
      template.resourceCountIs('AWS::CloudFront::OriginAccessControl', 1);
      template.hasResourceProperties('AWS::CloudFront::OriginAccessControl', {
        OriginAccessControlConfig: Match.objectLike({
          OriginAccessControlOriginType: 's3',
          SigningBehavior: 'always',
          SigningProtocol: 'sigv4',
        }),
      });
    });

    test('sets defaultRootObject, which the REST origin needs to resolve /', () => {
      // The S3 *website* endpoint resolved index.html implicitly; the REST endpoint
      // that OAC uses does not, so this property is load-bearing, not cosmetic.
      template.hasResourceProperties('AWS::CloudFront::Distribution', {
        DistributionConfig: Match.objectLike({ DefaultRootObject: 'index.html' }),
      });
    });

    test('no longer configures an S3 website endpoint', () => {
      const buckets = template.findResources('AWS::S3::Bucket');
      for (const bucket of Object.values(buckets)) {
        expect((bucket as any).Properties.WebsiteConfiguration).toBeUndefined();
      }
    });
  });

  describe('Hardening: link table is protected against data loss', () => {
    test('retains the table when the stack is deleted', () => {
      template.hasResource('AWS::DynamoDB::GlobalTable', {
        DeletionPolicy: 'Retain',
        UpdateReplacePolicy: 'Retain',
      });
    });

    test('enables deletion protection and point-in-time recovery', () => {
      template.hasResourceProperties('AWS::DynamoDB::GlobalTable', {
        Replicas: Match.arrayWith([
          Match.objectLike({
            DeletionProtectionEnabled: true,
            PointInTimeRecoverySpecification: { PointInTimeRecoveryEnabled: true },
          }),
        ]),
      });
    });

    test('the hosting bucket, by contrast, stays disposable', () => {
      // Deliberate asymmetry: website assets are reproducible from ./website,
      // user data is not.
      template.hasResource('AWS::S3::Bucket', {
        DeletionPolicy: 'Delete',
      });
    });
  });

  describe('Hardening: Lambdas run on Graviton with managed logging', () => {
    test('every application function targets arm64', () => {
      const functions = template.findResources('AWS::Lambda::Function', {
        Properties: { Runtime: 'provided.al2023' },
      });
      expect(Object.keys(functions)).toHaveLength(4);
      for (const fn of Object.values(functions)) {
        expect((fn as any).Properties.Architectures).toEqual(['arm64']);
      }
    });

    test('every application function emits JSON-structured logs', () => {
      const functions = template.findResources('AWS::Lambda::Function', {
        Properties: { Runtime: 'provided.al2023' },
      });
      for (const fn of Object.values(functions)) {
        expect((fn as any).Properties.LoggingConfig.LogFormat).toBe('JSON');
      }
    });

    test('every application function writes to an explicit log group', () => {
      const functions = template.findResources('AWS::Lambda::Function', {
        Properties: { Runtime: 'provided.al2023' },
      });
      for (const fn of Object.values(functions)) {
        expect((fn as any).Properties.LoggingConfig.LogGroup).toBeDefined();
      }
    });

    test('no application log group is left on infinite retention', () => {
      const groups = template.findResources('AWS::Logs::LogGroup');
      // At least one group per application function, all with a finite retention.
      expect(Object.keys(groups).length).toBeGreaterThanOrEqual(4);
      for (const group of Object.values(groups)) {
        expect((group as any).Properties.RetentionInDays).toBe(7);
      }
    });
  });

  describe('Kinesis analytics stream', () => {
    test('creates an on-demand stream with 24h retention', () => {
      template.resourceCountIs('AWS::Kinesis::Stream', 1);
      template.hasResourceProperties('AWS::Kinesis::Stream', {
        RetentionPeriodHours: 24,
        StreamModeDetails: { StreamMode: 'ON_DEMAND' },
      });
    });
  });

  describe('Observability', () => {
    test('gives processAnalytics an explicit log group with one week retention', () => {
      template.hasResourceProperties('AWS::Logs::LogGroup', {
        RetentionInDays: 7,
      });
    });

    test('alarms when too many invalid URLs are seen', () => {
      template.resourceCountIs('AWS::Logs::MetricFilter', 1);
      template.hasResourceProperties('AWS::CloudWatch::Alarm', {
        Threshold: 10,
        EvaluationPeriods: 1,
        ComparisonOperator: 'GreaterThanThreshold',
        TreatMissingData: 'notBreaching',
      });
    });
  });

  describe('DNS', () => {
    test('aliases the apex record at the distribution', () => {
      template.hasResourceProperties('AWS::Route53::RecordSet', {
        Name: 'krtk.rs.',
        Type: 'A',
      });
    });
  });
});

describe('CertificateStack', () => {
  test('issues a DNS-validated certificate for krtk.rs', () => {
    const app = new cdk.App();
    const stack = new CertificateStack(app, 'TestCertificateStack', {
      env: { ...TEST_ENV, region: 'us-east-1' },
    });
    const template = Template.fromStack(stack);

    template.resourceCountIs('AWS::CertificateManager::Certificate', 1);
    template.hasResourceProperties('AWS::CertificateManager::Certificate', {
      DomainName: 'krtk.rs',
      ValidationMethod: 'DNS',
    });
  });
});

describe('SecretsStack', () => {
  test('creates the Google API key secret', () => {
    const app = new cdk.App();
    const stack = new SecretsStack(app, 'TestSecretsStack', { env: TEST_ENV });
    const template = Template.fromStack(stack);

    template.resourceCountIs('AWS::SecretsManager::Secret', 1);
    template.hasResourceProperties('AWS::SecretsManager::Secret', {
      Description: 'Google API Key',
    });
  });
});
