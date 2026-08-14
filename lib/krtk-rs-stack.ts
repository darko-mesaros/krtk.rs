import * as cdk from 'aws-cdk-lib';
import { Construct } from 'constructs';
import { RustFunction } from 'cargo-lambda-cdk';
import { HttpApi, HttpMethod, CorsHttpMethod, HttpStage, ThrottleSettings } from 'aws-cdk-lib/aws-apigatewayv2';
import { HttpLambdaIntegration } from 'aws-cdk-lib/aws-apigatewayv2-integrations';
import { HostedZone, ARecord, RecordTarget } from 'aws-cdk-lib/aws-route53';
import { Certificate} from 'aws-cdk-lib/aws-certificatemanager';
import { TableV2, AttributeType, ProjectionType } from 'aws-cdk-lib/aws-dynamodb';
import { CloudFrontTarget } from 'aws-cdk-lib/aws-route53-targets';
import { Bucket, BlockPublicAccess } from 'aws-cdk-lib/aws-s3';
import { BucketDeployment, Source } from 'aws-cdk-lib/aws-s3-deployment';
import { Endpoint, RealtimeLogConfig, AllowedMethods, CachePolicy, Distribution, OriginProtocolPolicy, OriginRequestPolicy, ViewerProtocolPolicy } from 'aws-cdk-lib/aws-cloudfront';
import { HttpOrigin, S3BucketOrigin } from 'aws-cdk-lib/aws-cloudfront-origins';
import { Stream, StreamMode } from 'aws-cdk-lib/aws-kinesis';
import { KinesisEventSource } from 'aws-cdk-lib/aws-lambda-event-sources';
import { Architecture, LoggingFormat, StartingPosition } from 'aws-cdk-lib/aws-lambda';
import { FilterPattern, LogGroup, MetricFilter, RetentionDays } from 'aws-cdk-lib/aws-logs';
import { Alarm, ComparisonOperator, TreatMissingData } from 'aws-cdk-lib/aws-cloudwatch';
import { Secret } from 'aws-cdk-lib/aws-secretsmanager';

interface KrtkRsStackProps extends cdk.StackProps {
  certificateArn: string;
  googleApiKeySecret: Secret
}

export class KrtkRsStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: KrtkRsStackProps) {
    super(scope, id, props);

    // Import Cert
    const cert = Certificate.fromCertificateArn(this, 'ImportedCert', props.certificateArn);

    // Route53
    const hostedZone = HostedZone.fromHostedZoneAttributes(this, 'hostedZone',{
      zoneName: 'krtk.rs',
      hostedZoneId: 'Z07540833AST0TH4M5W39',
    })

    // S3 Hosting — private bucket, reachable only through CloudFront Origin Access Control.
    // removalPolicy stays DESTROY + autoDeleteObjects: the contents are reproducible assets
    // redeployed from ./website by the BucketDeployment below. That asymmetry with linkTable
    // (RETAIN) is deliberate: reproducible assets may be destroyed, user data may not.
    const hostingBucket = new Bucket(this, 'hostingBucket',{
      removalPolicy: cdk.RemovalPolicy.DESTROY,
      autoDeleteObjects: true,
      bucketName: 'krtk.rs',
      blockPublicAccess: BlockPublicAccess.BLOCK_ALL,
      enforceSSL: true,
      versioned: true,
      // No websiteIndexDocument: OAC talks to the S3 REST endpoint, not the website
      // endpoint, so index resolution comes from the distribution's defaultRootObject.
    });

    // One OAC-backed origin instance, shared by every S3 behaviour on the distribution.
    const s3Origin = S3BucketOrigin.withOriginAccessControl(hostingBucket);

    // Kinesis stream for analytics
    const cfAnalyticsStream = new Stream(this, 'cfAnalyticsStream', {
       streamMode: StreamMode.ON_DEMAND,
      retentionPeriod: cdk.Duration.hours(24)
    });

    // Real time Analytics streaming configuration
    const realTimeConfig = new RealtimeLogConfig(this, 'realTimeConfig',{
      endPoints: [
        Endpoint.fromKinesisStream(cfAnalyticsStream),
      ],
      fields: [
        'timestamp',
        'c-ip',
        'cs-uri-stem',
        'sc-status',
      ],
      realtimeLogConfigName: 'krtkAnalytics',
      samplingRate: 100,
    });

    // DynamoDB — this holds user data (and is about to hold per-user link ownership),
    // so it is protected against accidental teardown: RETAIN keeps the table if the stack
    // is deleted, deletionProtection blocks a direct DeleteTable, and PITR gives a
    // 35-day restore window.
    const linkDatabase = new TableV2(this, 'linkTable', {
      partitionKey: {
        name: 'LinkId',
        type: AttributeType.STRING
      },
      removalPolicy: cdk.RemovalPolicy.RETAIN,
      pointInTimeRecoverySpecification: {
        pointInTimeRecoveryEnabled: true,
      },
      deletionProtection: true,
    });
    linkDatabase.addGlobalSecondaryIndex({
      indexName: 'TimeStampIndex',
      partitionKey: {
        name: 'SortKey',
        type: AttributeType.STRING,
      },
      sortKey: {
        name: 'TimeStamp',
        type: AttributeType.NUMBER,
      },
      projectionType: ProjectionType.ALL
    })

    // Explicit, CDK-owned log groups for every function. Without these, Lambda creates the
    // group implicitly on first invocation with retention set to "Never expire", which is
    // both a cost leak and outside CloudFormation's control. Passing the group via the
    // `logGroup` prop links it to the function directly, so no name-matching or
    // addDependency() is needed. DESTROY is correct here: logs are disposable telemetry.
    const logGroupDefaults = {
      retention: RetentionDays.ONE_WEEK,
      removalPolicy: cdk.RemovalPolicy.DESTROY,
    };
    const createLinkLogGroup = new LogGroup(this, 'createLinkLogGroup', logGroupDefaults);
    const getLinksLogGroup = new LogGroup(this, 'getLinksLogGroup', logGroupDefaults);
    const visitLinkLogGroup = new LogGroup(this, 'visitLinkLogGroup', logGroupDefaults);
    const processAnalyticsLogGroup = new LogGroup(this, 'processAnalyticsLogGroup', logGroupDefaults);

    // 3x Lambda
    const createLinkLambda = new RustFunction(this, 'createLink', {
      manifestPath: 'lambda/create_link/Cargo.toml',
      runtime: 'provided.al2023',
      architecture: Architecture.ARM_64,
      timeout: cdk.Duration.seconds(30),
      logGroup: createLinkLogGroup,
      loggingFormat: LoggingFormat.JSON,
      environment: {
        TABLE_NAME: linkDatabase.tableName,
        SHORTENER_DOMAIN: 'krtk.rs',
      }
    });
    const getLinksLambda = new RustFunction(this, 'getLinks', {
      manifestPath: 'lambda/get_links/Cargo.toml',
      runtime: 'provided.al2023',
      architecture: Architecture.ARM_64,
      timeout: cdk.Duration.seconds(30),
      logGroup: getLinksLogGroup,
      loggingFormat: LoggingFormat.JSON,
      environment: {
        TABLE_NAME: linkDatabase.tableName,
        SHORTENER_DOMAIN: 'krtk.rs',
      }
    });
    const visitLinkLambda = new RustFunction(this, 'visitLink', {
      manifestPath: 'lambda/visit_link/Cargo.toml',
      runtime: 'provided.al2023',
      architecture: Architecture.ARM_64,
      timeout: cdk.Duration.seconds(45),
      logGroup: visitLinkLogGroup,
      loggingFormat: LoggingFormat.JSON,
      environment: {
        TABLE_NAME: linkDatabase.tableName,
        SHORTENER_DOMAIN: 'krtk.rs',
      }
    });
    // Table permissions
    linkDatabase.grantReadData(getLinksLambda);
    linkDatabase.grantReadData(visitLinkLambda);
    linkDatabase.grantWriteData(createLinkLambda);

    // Secrets permissions
    props.googleApiKeySecret.grantRead(createLinkLambda);

    // Append secret
    createLinkLambda.addEnvironment('GOOGLE_API_KEY_SECRET', props.googleApiKeySecret.secretArn);

    const processAnalyticsLambda = new RustFunction(this, 'processAnalyticsLambda', {
      manifestPath: 'lambda/process_analytics/Cargo.toml',
      runtime: 'provided.al2023',
      architecture: Architecture.ARM_64,
      timeout: cdk.Duration.seconds(30),
      logGroup: processAnalyticsLogGroup,
      loggingFormat: LoggingFormat.JSON,
      environment: {
        TABLE_NAME: linkDatabase.tableName,
        SHORTENER_DOMAIN: 'krtk.rs',
      }
    });
    // Give Function permission to Kinesis
    cfAnalyticsStream.grantRead(processAnalyticsLambda);
    // ESM for Kinesis
    processAnalyticsLambda.addEventSource(new KinesisEventSource(cfAnalyticsStream,{
      batchSize: 1,
      startingPosition: StartingPosition.TRIM_HORIZON,
    }));
    linkDatabase.grantWriteData(processAnalyticsLambda);

    // HTTP Api
    const api = new HttpApi(this, 'httpApi',{
      apiName: 'krkt-rs-link-shortener',
      createDefaultStage: false,
      corsPreflight: {
        allowHeaders: ['content-type'],
        allowMethods: [
          CorsHttpMethod.GET,
          CorsHttpMethod.POST,
          CorsHttpMethod.OPTIONS,
        ],
        allowOrigins: ['*'],
        maxAge: cdk.Duration.days(10),
      }
    });

    // Prod Throttle settings
    const prodThrottle: ThrottleSettings = {
      rateLimit: 5, // 5 requests per second
      burstLimit: 10, // 10 concurrent requests max
    }

    // Prod Stage
    new HttpStage(this, 'prodStage', {
      httpApi: api,
      stageName: 'prod',
      description: 'Production stage',
      throttle: prodThrottle,
      autoDeploy: true,
    });

    // Integrations
    const createLinkInteg = new HttpLambdaIntegration('createLinkInteg', createLinkLambda);
    api.addRoutes({
      path: '/api/links',
      methods: [HttpMethod.POST],
      integration: createLinkInteg
    });
    const getLinksInteg = new HttpLambdaIntegration('getLinksInteg', getLinksLambda);
    api.addRoutes({
      path: '/api/links',
      methods: [HttpMethod.GET],
      integration: getLinksInteg
    });
    const visitLinkInteg = new HttpLambdaIntegration('visitLinkInteg', visitLinkLambda);
    api.addRoutes({
      path: '/{linkId}',
      methods: [HttpMethod.GET],
      integration: visitLinkInteg
    });

    // CF
    const cdn = new Distribution(this, 'websiteCdn',{
      domainNames: ['krtk.rs'],
      // Required with an OAC/REST origin: the S3 REST endpoint does not resolve index
      // documents the way the website endpoint did, so '/' must be mapped explicitly.
      defaultRootObject: 'index.html',
      defaultBehavior: {
        origin: s3Origin,
        viewerProtocolPolicy: ViewerProtocolPolicy.REDIRECT_TO_HTTPS,
        cachePolicy: CachePolicy.CACHING_OPTIMIZED,
        originRequestPolicy: OriginRequestPolicy.CORS_S3_ORIGIN,
      },
      additionalBehaviors: {
        '/api/*': {
          origin: new HttpOrigin(`${api.apiId}.execute-api.${this.region}.amazonaws.com`,{
            originPath: '/prod',
            protocolPolicy: OriginProtocolPolicy.HTTPS_ONLY,
          }),
          viewerProtocolPolicy: ViewerProtocolPolicy.REDIRECT_TO_HTTPS,
          allowedMethods: AllowedMethods.ALLOW_ALL,
          originRequestPolicy: OriginRequestPolicy.ALL_VIEWER_EXCEPT_HOST_HEADER,
          cachePolicy: CachePolicy.CACHING_DISABLED,
        },
        '/assets/*': {
          origin: s3Origin,
          viewerProtocolPolicy: ViewerProtocolPolicy.REDIRECT_TO_HTTPS,
          cachePolicy: CachePolicy.CACHING_OPTIMIZED,
          originRequestPolicy: OriginRequestPolicy.CORS_S3_ORIGIN,
        },
        // Load-bearing carve-out, do not remove. In a CloudFront path pattern '?'
        // matches exactly one character, so the '/?*' link-redirect behaviour below
        // matches EVERY root-level path with at least one character -- including
        // '/index.html'. With the old S3 website origin that never mattered, because
        // the website endpoint resolved the index document at the origin and no
        // CloudFront-level rewrite occurred. OAC uses the S3 REST endpoint, which
        // cannot do that, so defaultRootObject has to rewrite '/' -> '/index.html' --
        // and without this behaviour that rewritten path falls through to '/?*' and
        // gets served by the visit_link Lambda, which 404s it as an unknown link id.
        '/index.html': {
          origin: s3Origin,
          viewerProtocolPolicy: ViewerProtocolPolicy.REDIRECT_TO_HTTPS,
          cachePolicy: CachePolicy.CACHING_OPTIMIZED,
          originRequestPolicy: OriginRequestPolicy.CORS_S3_ORIGIN,
        },
        '/terms': {
          origin: s3Origin,
          viewerProtocolPolicy: ViewerProtocolPolicy.REDIRECT_TO_HTTPS,
          cachePolicy: CachePolicy.CACHING_OPTIMIZED,
          originRequestPolicy: OriginRequestPolicy.CORS_S3_ORIGIN,
          responseHeadersPolicy: new cdk.aws_cloudfront.ResponseHeadersPolicy(this, 'TermsResponseHeaders', {
            customHeadersBehavior: {
             customHeaders: [
               { header: 'content-type', value: 'text/html; charset=utf-8', override: true}
             ]
            }
          }),
        },
        '/privacy': {
          origin: s3Origin,
          viewerProtocolPolicy: ViewerProtocolPolicy.REDIRECT_TO_HTTPS,
          cachePolicy: CachePolicy.CACHING_OPTIMIZED,
          originRequestPolicy: OriginRequestPolicy.CORS_S3_ORIGIN,
          responseHeadersPolicy: new cdk.aws_cloudfront.ResponseHeadersPolicy(this, 'PrivacyResponseHeaders', {
            customHeadersBehavior: {
             customHeaders: [
               { header: 'content-type', value: 'text/html; charset=utf-8', override: true}
             ]
            }
          }),
        },
        '/?*': {
          origin: new HttpOrigin(`${api.apiId}.execute-api.${this.region}.amazonaws.com`,{
            originPath: '/prod',
            protocolPolicy: OriginProtocolPolicy.HTTPS_ONLY,

          }),
          viewerProtocolPolicy: ViewerProtocolPolicy.REDIRECT_TO_HTTPS,
          allowedMethods: AllowedMethods.ALLOW_ALL,
          cachePolicy: CachePolicy.CACHING_DISABLED,
          originRequestPolicy: OriginRequestPolicy.ALL_VIEWER_EXCEPT_HOST_HEADER,
          realtimeLogConfig: realTimeConfig,
        },
      },
      certificate: cert,
      errorResponses: [
        {
          httpStatus: 404,
          responseHttpStatus: 404,
          // '/index.html', not '/': pointing the error page at the bare root made the
          // error page fetch itself resolve through defaultRootObject and fall into
          // the '/?*' behaviour, so a 404 produced a second 404 with an empty body.
          responsePagePath: '/index.html'
        }
      ]
    });

    new BucketDeployment(this, 'deployWebsite',{
      sources: [Source.asset('./website')],
      destinationBucket: hostingBucket,
      distribution: cdn,
      distributionPaths: ['/*']
    });

    new ARecord(this, 'apiAliasRecord',{
      zone: hostedZone,
      target: RecordTarget.fromAlias(
        new CloudFrontTarget(cdn)
      ),
      recordName: 'krtk.rs'
    });

    // METRICS - CLOUDWATCH
    // processAnalyticsLogGroup is declared above and attached to the function via its
    // `logGroup` prop, so the metric filter can bind to it directly without the
    // addDependency() workaround the implicit-log-group arrangement needed.
    const invalidUrlMetricFilter = new MetricFilter(this, 'invalidUrlMetricFilter', {
      logGroup: processAnalyticsLogGroup,
      filterPattern: FilterPattern.stringValue('$.level', '=', 'warn'),
      metricNamespace: 'KrtkRs',
      metricName: 'InvalidUrlWarnings',
      defaultValue: 0,
    });
    invalidUrlMetricFilter.node.addDependency(processAnalyticsLogGroup);

    const invalidUrlAlarm = new Alarm(this, 'invalidUrlAlarm',{
      metric: invalidUrlMetricFilter.metric({
        period: cdk.Duration.minutes(5),
        statistic: 'Sum',
      }),
      threshold: 10,
      evaluationPeriods: 1,
      comparisonOperator: ComparisonOperator.GREATER_THAN_THRESHOLD,
      treatMissingData: TreatMissingData.NOT_BREACHING,
      alarmDescription: 'Alarm when too many invalid URLs are processed'
    });

    // Outputs
    new cdk.CfnOutput(this, 'distributionId',{
      value: cdn.distributionId,
      description: 'CDN ID'

    });
  }
}
