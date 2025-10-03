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
import { HttpOrigin, S3StaticWebsiteOrigin } from 'aws-cdk-lib/aws-cloudfront-origins';
import { Stream, StreamMode } from 'aws-cdk-lib/aws-kinesis';
import { KinesisEventSource } from 'aws-cdk-lib/aws-lambda-event-sources';
import { StartingPosition } from 'aws-cdk-lib/aws-lambda';
import { FilterPattern, LogGroup, MetricFilter } from 'aws-cdk-lib/aws-logs';
import { Alarm, ComparisonOperator, TreatMissingData } from 'aws-cdk-lib/aws-cloudwatch';

interface KrtkRsStackProps extends cdk.StackProps {
  certificateArn: string;
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

    // S3 Hosting
    const hostingBucket = new Bucket(this, 'hostingBucket',{
      removalPolicy: cdk.RemovalPolicy.DESTROY,
      autoDeleteObjects: true,
      bucketName: 'krtk.rs',
      publicReadAccess: true,
      blockPublicAccess: new BlockPublicAccess({
        blockPublicAcls: false,
        blockPublicPolicy: false,
        ignorePublicAcls: false,
        restrictPublicBuckets: false,
      }),
      versioned: true,
      websiteIndexDocument: 'index.html'
    });

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

    // DynamoDB
    const linkDatabase = new TableV2(this, 'linkTable', {
      partitionKey: {
        name: 'LinkId',
        type: AttributeType.STRING
      },
      removalPolicy: cdk.RemovalPolicy.DESTROY, // TODO: REMOVE FOR PROD
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

    // 3x Lambda
    const createLinkLambda = new RustFunction(this, 'createLink', {
      manifestPath: 'lambda/create_link/Cargo.toml',
      runtime: 'provided.al2023',
      timeout: cdk.Duration.seconds(30),
      environment: {
        TABLE_NAME: linkDatabase.tableName,
        SHORTENER_DOMAIN: 'krtk.rs',
      }
    });
    const getLinksLambda = new RustFunction(this, 'getLinks', {
      manifestPath: 'lambda/get_links/Cargo.toml',
      runtime: 'provided.al2023',
      timeout: cdk.Duration.seconds(30),
      environment: {
        TABLE_NAME: linkDatabase.tableName,
        SHORTENER_DOMAIN: 'krtk.rs',
      }
    });
    const visitLinkLambda = new RustFunction(this, 'visitLink', {
      manifestPath: 'lambda/visit_link/Cargo.toml',
      runtime: 'provided.al2023',
      timeout: cdk.Duration.seconds(45),
      environment: {
        TABLE_NAME: linkDatabase.tableName,
        SHORTENER_DOMAIN: 'krtk.rs',
      }
    });
    // Table permissions
    linkDatabase.grantReadData(getLinksLambda);
    linkDatabase.grantReadData(visitLinkLambda);
    linkDatabase.grantWriteData(createLinkLambda);

    const processAnalyticsLambda = new RustFunction(this, 'processAnalyticsLambda', {
      manifestPath: 'lambda/process_analytics/Cargo.toml',
      runtime: 'provided.al2023',
      timeout: cdk.Duration.seconds(30),
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
      defaultBehavior: {
        origin: new S3StaticWebsiteOrigin(hostingBucket),
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
          origin: new S3StaticWebsiteOrigin(hostingBucket),
          viewerProtocolPolicy: ViewerProtocolPolicy.REDIRECT_TO_HTTPS,
          cachePolicy: CachePolicy.CACHING_OPTIMIZED,
          originRequestPolicy: OriginRequestPolicy.CORS_S3_ORIGIN,
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
          responsePagePath: '/'
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

    // METRICS - CLOUDWATCH - Needed because the log group is not created during first run.
    const processAnalyticsLogGroup = new LogGroup(this, 'processAnalyticsLogGroup',{
      logGroupName: `/aws/lambda/${processAnalyticsLambda.functionName}`,
      retention: cdk.aws_logs.RetentionDays.ONE_WEEK,
      removalPolicy: cdk.RemovalPolicy.DESTROY,
    });

    // const processAnalyticsLogGroup = LogGroup.fromLogGroupName(this, 'processAnalyticsLogGroup',`/aws/lambda/${processAnalyticsLambda.functionName}`);
    processAnalyticsLogGroup.node.addDependency(processAnalyticsLambda);

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
  }
}
