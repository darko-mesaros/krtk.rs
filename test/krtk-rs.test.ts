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
    authCertificateArn: certStack.authCertificate.certificateArn,
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
      // Two tables now: the link table and the API key table. Pinning the count keeps
      // an accidental third table visible rather than silently deployed.
      template.resourceCountIs('AWS::DynamoDB::GlobalTable', 2);
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
    test('creates the six application functions on provided.al2023', () => {
      // The stack also synthesizes CDK-managed helper functions (bucket
      // deployment, auto-delete-objects), so assert on the custom runtime
      // rather than a bare resourceCountIs over every function.
      const functions = template.findResources('AWS::Lambda::Function', {
        Properties: { Runtime: 'provided.al2023' },
      });
      // Six now: the four link functions plus the authorizer and manage_keys.
      expect(Object.keys(functions)).toHaveLength(6);
    });

    test('every LINK function receives TABLE_NAME and SHORTENER_DOMAIN', () => {
      // Scoped to the link functions: the authorizer and manage_keys deliberately have
      // no access to the link table, so asserting over every function would either fail
      // or pressure someone into granting them access they should not have.
      const functions = template.findResources('AWS::Lambda::Function', {
        Properties: { Runtime: 'provided.al2023' },
      });

      const linkFunctions = Object.values(functions).filter(
        (fn) => (fn as any).Properties.Environment.Variables.TABLE_NAME !== undefined,
      );
      expect(linkFunctions).toHaveLength(4);

      for (const fn of linkFunctions) {
        const env = (fn as any).Properties.Environment.Variables;
        expect(env.SHORTENER_DOMAIN).toBe('krtk.rs');
      }
    });

    test('the auth functions get the key table and NOT the link table', () => {
      const functions = template.findResources('AWS::Lambda::Function', {
        Properties: { Runtime: 'provided.al2023' },
      });
      const authFunctions = Object.values(functions).filter(
        (fn) => (fn as any).Properties.Environment.Variables.API_KEY_TABLE_NAME !== undefined,
      );
      expect(authFunctions).toHaveLength(2);
      for (const fn of authFunctions) {
        expect((fn as any).Properties.Environment.Variables.TABLE_NAME).toBeUndefined();
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

    test('exposes exactly the six expected routes', () => {
      const routes = template.findResources('AWS::ApiGatewayV2::Route');
      const routeKeys = Object.values(routes).map((r) => (r as any).Properties.RouteKey).sort();
      expect(routeKeys).toEqual([
        'DELETE /api/keys/{keyId}',
        'GET /api/keys',
        'GET /api/links',
        'GET /{linkId}',
        'POST /api/keys',
        'POST /api/links',
      ]);
    });

    /**
     * The FR-4.4 security boundary, asserted structurally.
     *
     * /api/links uses the CUSTOM Lambda authorizer because it must accept a JWT or an
     * API key. /api/keys uses the NATIVE user pool authorizer, which only understands
     * JWTs -- that is what stops a leaked API key from minting its own replacements,
     * and it holds without any handler-side conditional. If someone ever points both
     * routes at one authorizer, this fails.
     */
    test('links accept either credential; key management is JWT-only', () => {
      const authorizers = template.findResources('AWS::ApiGatewayV2::Authorizer');
      const byType: Record<string, string> = {};
      for (const [logicalId, a] of Object.entries(authorizers)) {
        byType[logicalId] = (a as any).Properties.AuthorizerType;
      }
      expect(Object.values(byType).sort()).toEqual(['JWT', 'REQUEST']);

      const jwtId = Object.keys(byType).find((k) => byType[k] === 'JWT')!;
      const requestId = Object.keys(byType).find((k) => byType[k] === 'REQUEST')!;

      const routes = template.findResources('AWS::ApiGatewayV2::Route');
      for (const route of Object.values(routes)) {
        const props = (route as any).Properties;
        const key: string = props.RouteKey;
        const refId = props.AuthorizerId?.Ref;

        if (key.includes('/api/keys')) {
          expect(refId).toBe(jwtId);
        } else if (key.includes('/api/links')) {
          expect(refId).toBe(requestId);
        } else {
          // The public redirect must carry no authorizer at all.
          expect(props.AuthorizerId).toBeUndefined();
          expect(props.AuthorizationType ?? 'NONE').toBe('NONE');
        }
      }
    });

    /**
     * Zero cache is a requirement, not a default. Any TTL leaves a window in which a
     * revoked API key still authorizes requests, but FR-4.6 says revocation takes
     * effect on the next call.
     */
    test('the links authorizer does not cache its result', () => {
      template.hasResourceProperties('AWS::ApiGatewayV2::Authorizer', {
        AuthorizerType: 'REQUEST',
        AuthorizerResultTtlInSeconds: 0,
      });
    });

    /**
     * Regression guard for the 401-before-invocation outage.
     *
     * Every declared identity source is REQUIRED by an HTTP API: a request missing any
     * one of them is rejected with 401 before the authorizer Lambda is called. Declaring
     * both Authorization and X-Api-Key therefore demanded both credentials at once and
     * broke every real client, browser and script alike. The either/or contract of
     * FR-4.3 only holds if the authorizer is always invoked, which means no identity
     * sources at all -- so this asserts the absence, not a particular value.
     */
    test('the links authorizer declares no identity sources, so it always runs', () => {
      const authorizers = template.findResources('AWS::ApiGatewayV2::Authorizer', {
        Properties: { AuthorizerType: 'REQUEST' },
      });
      expect(Object.keys(authorizers)).toHaveLength(1);
      const props = (Object.values(authorizers)[0] as any).Properties;
      expect(props.IdentitySource ?? []).toEqual([]);
    });

    test('restricts CORS to the krtk.rs origin and allows the auth headers', () => {
      template.hasResourceProperties('AWS::ApiGatewayV2::Api', {
        CorsConfiguration: Match.objectLike({
          AllowMethods: Match.arrayWith(['GET', 'POST', 'OPTIONS']),
          AllowHeaders: Match.arrayWith(['content-type', 'authorization', 'x-api-key']),
          AllowOrigins: ['https://krtk.rs'],
        }),
      });
    });

    test('never allows a wildcard CORS origin', () => {
      // Requests now carry credentials, so a wildcard origin would let any site read
      // an authenticated response.
      const apis = template.findResources('AWS::ApiGatewayV2::Api');
      for (const api of Object.values(apis)) {
        const origins = (api as any).Properties.CorsConfiguration?.AllowOrigins ?? [];
        expect(origins).not.toContain('*');
      }
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
      expect(patterns).toEqual(['/?*', '/api/*', '/assets/*', '/auth/*', '/index.html', '/privacy', '/terms']);
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

    test('forces text/html on the extensionless /terms, /privacy and callback objects', () => {
      template.resourceCountIs('AWS::CloudFront::ResponseHeadersPolicy', 3);
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
      // Six now: the four link functions plus the authorizer and manage_keys.
      expect(Object.keys(functions)).toHaveLength(6);
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

  describe('Cognito user pool', () => {
    test('requires MFA with TOTP only, never SMS', () => {
      // SMS MFA is phishable, costs money per message, and adds a delivery dependency
      // to every login. Asserting its absence keeps a console-side "convenience" change
      // from silently weakening 2FA.
      template.hasResourceProperties('AWS::Cognito::UserPool', {
        MfaConfiguration: 'ON',
        EnabledMfas: ['SOFTWARE_TOKEN_MFA'],
      });
    });

    test('disables self-registration at the pool level, not just in the UI', () => {
      // This is what makes the sign-up API itself refuse, rather than merely hiding a
      // link on the hosted page.
      template.hasResourceProperties('AWS::Cognito::UserPool', {
        AdminCreateUserConfig: Match.objectLike({
          AllowAdminCreateUserOnly: true,
        }),
      });
    });

    test('enables deletion protection, since a pool has no point-in-time recovery', () => {
      template.hasResourceProperties('AWS::Cognito::UserPool', {
        DeletionProtection: 'ACTIVE',
      });
    });

    test('uses the Essentials feature plan, not Plus', () => {
      // Essentials is required for refresh token rotation and managed login, and is free
      // up to 10k MAU. Plus has NO free tier and bills from the first user while adding
      // only threat protection this project does not use.
      template.hasResourceProperties('AWS::Cognito::UserPool', {
        UserPoolTier: 'ESSENTIALS',
      });
    });

    test('signs in by email', () => {
      template.hasResourceProperties('AWS::Cognito::UserPool', {
        UsernameAttributes: Match.arrayWith(['email']),
      });
    });

    /**
     * Regression guard for a live outage.
     *
     * The domain uses managed login version 2, which renders its pages FROM a branding
     * style. A pool on MLV 2 with no ManagedLoginBranding resource serves "Login pages
     * unavailable -- Please contact an administrator" instead of a login form, while the
     * domain still reports ACTIVE and the certificate validates -- so it reads as a
     * propagation delay rather than a missing resource. Shipping MLV 2 without branding
     * is a broken login page, not a cosmetic gap.
     */
    test('managed login v2 has the branding resource it requires', () => {
      const domains = template.findResources('AWS::Cognito::UserPoolDomain');
      const usesV2 = Object.values(domains).some(
        (d) => (d as any).Properties.ManagedLoginVersion === 2,
      );

      if (usesV2) {
        template.resourceCountIs('AWS::Cognito::ManagedLoginBranding', 1);
        template.hasResourceProperties('AWS::Cognito::ManagedLoginBranding', {
          UseCognitoProvidedValues: true,
        });
      }
    });
  });

  describe('Cognito app client', () => {
    test('is a public client with no secret', () => {
      // A browser cannot keep a secret; PKCE is the protection instead.
      template.hasResourceProperties('AWS::Cognito::UserPoolClient', {
        GenerateSecret: false,
      });
    });

    test('allows the authorization code flow and NOT the implicit flow', () => {
      // Implicit would put tokens in the URL fragment, where they reach history and
      // referrer headers.
      template.hasResourceProperties('AWS::Cognito::UserPoolClient', {
        AllowedOAuthFlows: ['code'],
      });

      const clients = template.findResources('AWS::Cognito::UserPoolClient');
      for (const client of Object.values(clients)) {
        expect(client.Properties.AllowedOAuthFlows).not.toContain('implicit');
      }
    });

    test('restricts callback and logout URLs to the krtk.rs origin', () => {
      template.hasResourceProperties('AWS::Cognito::UserPoolClient', {
        CallbackURLs: ['https://krtk.rs/auth/callback'],
        LogoutURLs: ['https://krtk.rs/'],
      });
    });

    test('issues short-lived access tokens with revocation enabled', () => {
      template.hasResourceProperties('AWS::Cognito::UserPoolClient', {
        AccessTokenValidity: 60,
        IdTokenValidity: 60,
        RefreshTokenValidity: 43200,
        TokenValidityUnits: Match.objectLike({ AccessToken: 'minutes' }),
        EnableTokenRevocation: true,
      });
    });
  });

  describe('API key table', () => {
    test('is keyed on the key hash, so verification is a GetItem and never a scan', () => {
      template.hasResourceProperties('AWS::DynamoDB::GlobalTable', {
        KeySchema: [{ AttributeName: 'KeyHash', KeyType: 'HASH' }],
      });
    });

    test('indexes keys by owner for listing and the per-user cap', () => {
      template.hasResourceProperties('AWS::DynamoDB::GlobalTable', {
        KeySchema: [{ AttributeName: 'KeyHash', KeyType: 'HASH' }],
        GlobalSecondaryIndexes: Match.arrayWith([
          Match.objectLike({
            IndexName: 'OwnerIndex',
            KeySchema: [
              { AttributeName: 'OwnerId', KeyType: 'HASH' },
              { AttributeName: 'CreatedAt', KeyType: 'RANGE' },
            ],
          }),
        ]),
      });
    });

    /**
     * Deliberately asymmetric with linkTable, which DOES have PITR.
     *
     * Restoring a credential store resurrects revoked keys: revoke a leaked key today,
     * restore yesterday's snapshot, and the leaked credential works again with no
     * signal. Link data is the opposite — a restore there is pure recovery. This test
     * exists so the inconsistency cannot be "tidied up" without reading why.
     */
    test('has point-in-time recovery OFF, so a restore cannot revive revoked keys', () => {
      const tables = template.findResources('AWS::DynamoDB::GlobalTable');
      const keyTable = Object.values(tables).find(
        (t) => t.Properties?.KeySchema?.[0]?.AttributeName === 'KeyHash',
      );

      expect(keyTable).toBeDefined();

      const replicas = keyTable!.Properties.Replicas ?? [];
      for (const replica of replicas) {
        const pitr = replica.PointInTimeRecoverySpecification;
        expect(pitr?.PointInTimeRecoveryEnabled ?? false).toBe(false);
      }
    });

    test('is retained and deletion-protected, since losing it breaks every CLI', () => {
      const tables = template.findResources('AWS::DynamoDB::GlobalTable');
      const keyTable = Object.entries(tables).find(
        ([, t]) => t.Properties?.KeySchema?.[0]?.AttributeName === 'KeyHash',
      );

      expect(keyTable).toBeDefined();
      expect(keyTable![1].DeletionPolicy).toBe('Retain');

      // On GlobalTable (TableV2) both deletion protection and PITR are per-replica
      // properties, not top-level ones.
      const replicas = keyTable![1].Properties.Replicas ?? [];
      expect(replicas.length).toBeGreaterThan(0);
      for (const replica of replicas) {
        expect(replica.DeletionProtectionEnabled).toBe(true);
      }
    });

    test('expires keys via TTL for cleanup only', () => {
      // Rejection is enforced in code against ExpiresAt. TTL deletion can lag by days,
      // so it is a janitor, never the gate.
      template.hasResourceProperties('AWS::DynamoDB::GlobalTable', {
        KeySchema: [{ AttributeName: 'KeyHash', KeyType: 'HASH' }],
        TimeToLiveSpecification: { AttributeName: 'ExpiresAt', Enabled: true },
      });
    });
  });
});

describe('CertificateStack', () => {
  test('issues DNS-validated certificates for krtk.rs and the auth domain', () => {
    const app = new cdk.App();
    const stack = new CertificateStack(app, 'TestCertificateStack', {
      env: { ...TEST_ENV, region: 'us-east-1' },
    });
    const template = Template.fromStack(stack);

    template.resourceCountIs('AWS::CertificateManager::Certificate', 2);
    template.hasResourceProperties('AWS::CertificateManager::Certificate', {
      DomainName: 'krtk.rs',
      ValidationMethod: 'DNS',
    });
    template.hasResourceProperties('AWS::CertificateManager::Certificate', {
      DomainName: 'auth.krtk.rs',
      ValidationMethod: 'DNS',
    });
  });

  /**
   * Cognito requires a custom-domain certificate in us-east-1 regardless of the user
   * pool's own region. This stack is the only us-east-1 stack in the app, which is why
   * the auth cert lives here rather than beside the pool in us-west-2.
   */
  test('is pinned to us-east-1, which Cognito requires for a custom domain cert', () => {
    const app = new cdk.App();
    const stack = new CertificateStack(app, 'TestCertificateStack', {
      env: { ...TEST_ENV, region: 'us-east-1' },
    });

    expect(stack.region).toBe('us-east-1');
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
