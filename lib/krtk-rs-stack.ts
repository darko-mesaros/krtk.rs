import * as cdk from 'aws-cdk-lib';
import { Construct } from 'constructs';
import { RustFunction } from 'cargo-lambda-cdk';
import { HttpApi, HttpMethod, CorsHttpMethod, HttpStage, ThrottleSettings } from 'aws-cdk-lib/aws-apigatewayv2';
import { HttpLambdaIntegration } from 'aws-cdk-lib/aws-apigatewayv2-integrations';
import {
  HttpLambdaAuthorizer,
  HttpLambdaResponseType,
  HttpUserPoolAuthorizer,
} from 'aws-cdk-lib/aws-apigatewayv2-authorizers';
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
import {
  AccountRecovery,
  FeaturePlan,
  Mfa,
  OAuthScope,
  UserPool,
} from 'aws-cdk-lib/aws-cognito';
import { UserPoolDomainTarget } from 'aws-cdk-lib/aws-route53-targets';

/** The public site. */
const SITE_DOMAIN = 'krtk.rs';
/** Where the Cognito Hosted UI is served, so a password is never typed into an AWS hostname. */
const AUTH_DOMAIN = `auth.${SITE_DOMAIN}`;

interface KrtkRsStackProps extends cdk.StackProps {
  certificateArn: string;
  /** ACM cert for auth.krtk.rs. Must be us-east-1 — a Cognito requirement independent of the pool's region. */
  authCertificateArn: string;
  googleApiKeySecret: Secret
}

export class KrtkRsStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: KrtkRsStackProps) {
    super(scope, id, props);

    // Import Cert
    const cert = Certificate.fromCertificateArn(this, 'ImportedCert', props.certificateArn);
    const authCert = Certificate.fromCertificateArn(this, 'ImportedAuthCert', props.authCertificateArn);

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

    // ---------------------------------------------------------------------------
    // Authentication
    // ---------------------------------------------------------------------------

    // The pool lives here rather than in its own stack: its custom domain needs the
    // us-east-1 certificate from CertificateStack, and that cross-region wiring already
    // exists. A third stack would add another cross-stack reference for no isolation gain.
    const userPool = new UserPool(this, 'userPool', {
      userPoolName: 'krtk-rs-users',
      // Email is the sign-in alias; users never see or choose a username.
      signInAliases: { email: true },
      signInCaseSensitive: false,
      autoVerify: { email: true },
      // Invite-only. Self-registration is off at the POOL level, not merely hidden in
      // the UI, so the sign-up API is rejected too (FR-1.4).
      selfSignUpEnabled: false,
      userInvitation: {
        emailSubject: 'Your krtk.rs account',
        emailBody: 'Your krtk.rs username is {username} and the temporary password is {####}',
      },
      // TOTP only. SMS is deliberately not enabled: it is phishable, costs money per
      // message via SNS, and adds a delivery dependency to every login (FR-1.3).
      mfa: Mfa.REQUIRED,
      mfaSecondFactor: { sms: false, otp: true },
      passwordPolicy: {
        minLength: 12,
        requireLowercase: true,
        requireUppercase: true,
        requireDigits: true,
        requireSymbols: true,
      },
      accountRecovery: AccountRecovery.EMAIL_ONLY,
      // Deleting the pool destroys every identity and TOTP enrolment, and unlike the
      // link table there is no point-in-time recovery for a user pool.
      removalPolicy: cdk.RemovalPolicy.RETAIN,
      deletionProtection: true,
      // Essentials is required for refresh token rotation (FR-1.8) and managed login.
      // NOT Plus: Plus has no free tier and bills from the first user, and only adds
      // threat protection this project does not use.
      featurePlan: FeaturePlan.ESSENTIALS,
    });

    const userPoolClient = userPool.addClient('webClient', {
      userPoolClientName: 'krtk-rs-web',
      // Public client: a browser cannot keep a secret, so PKCE is the protection
      // instead of a client secret.
      generateSecret: false,
      authFlows: { userSrp: true },
      oAuth: {
        flows: {
          authorizationCodeGrant: true,
          // Implicit would put tokens in the URL fragment, where they land in history
          // and referrers. PKCE + code exchange keeps them out of the address bar.
          implicitCodeGrant: false,
        },
        scopes: [OAuthScope.OPENID, OAuthScope.EMAIL, OAuthScope.PROFILE],
        callbackUrls: [`https://${SITE_DOMAIN}/auth/callback`],
        logoutUrls: [`https://${SITE_DOMAIN}/`],
      },
      accessTokenValidity: cdk.Duration.hours(1),
      idTokenValidity: cdk.Duration.hours(1),
      refreshTokenValidity: cdk.Duration.days(30),
      enableTokenRevocation: true,
      preventUserExistenceErrors: true,
    });

    // Hosted UI on our own hostname, so a password is never typed into an
    // amazoncognito.com URL.
    //
    // Cognito provisions its OWN CloudFront distribution for this, and that provisioning
    // is not finished when `cdk deploy` reports success. Verify the login page actually
    // serves before assuming this step is done.
    const userPoolDomain = userPool.addDomain('authDomain', {
      customDomain: {
        domainName: AUTH_DOMAIN,
        certificate: authCert,
      },
      managedLoginVersion: cdk.aws_cognito.ManagedLoginVersion.NEWER_MANAGED_LOGIN,
    });

    // REQUIRED by managed login version 2. Version 2 renders its pages FROM a branding
    // style, so a pool on MLV 2 with no branding resource serves "Login pages
    // unavailable -- Please contact an administrator" instead of a login form. The domain
    // still reports ACTIVE and the certificate is fine, which makes it look like a
    // propagation delay rather than a missing resource.
    //
    // `useCognitoProvidedValues: true` takes Cognito's stock styling. Replace it with
    // `settings`/`assets` only if the login page needs krtk.rs branding.
    const loginBranding = new cdk.aws_cognito.CfnManagedLoginBranding(this, 'authLoginBranding', {
      userPoolId: userPool.userPoolId,
      clientId: userPoolClient.userPoolClientId,
      useCognitoProvidedValues: true,
    });
    // Branding is per (pool, client) but is served through the domain, so make the
    // dependency explicit rather than relying on synthesis order.
    loginBranding.node.addDependency(userPoolDomain);

    // Cognito requires the APEX domain to have a resolvable A record before it will
    // accept a custom subdomain. That is already satisfied by the site's own alias
    // record further down -- do not remove it.
    new ARecord(this, 'authAliasRecord', {
      zone: hostedZone,
      recordName: AUTH_DOMAIN,
      target: RecordTarget.fromAlias(new UserPoolDomainTarget(userPoolDomain)),
    });

    // API keys: krtk.rs-owned credentials, not Cognito ones. Cognito authenticates the
    // human who mints and revokes them; these authenticate the script.
    //
    // A separate table rather than single-table design in linkTable, whose partition key
    // is literally named `LinkId` -- storing key rows there would make that name a lie
    // and bind two unrelated entities to one retention policy.
    const apiKeyTable = new TableV2(this, 'apiKeyTable', {
      // The SHA-256 hash IS the lookup key, so verification is one GetItem with no scan.
      // The plaintext key is never stored.
      partitionKey: {
        name: 'KeyHash',
        type: AttributeType.STRING,
      },
      removalPolicy: cdk.RemovalPolicy.RETAIN,
      deletionProtection: true,
      // NOTE: point-in-time recovery is deliberately OFF here, unlike linkTable.
      //
      // This is a credential store. A restore would RESURRECT REVOKED KEYS: revoke a
      // leaked key today, restore from yesterday's snapshot, and the leaked credential
      // is valid again with no signal that it happened. For link data a restore is pure
      // recovery; for keys it is a rollback of security decisions.
      //
      // The data is also trivially reconstructible -- a user re-mints in seconds,
      // whereas a shortened link cannot be re-derived. Do NOT "fix" this asymmetry
      // without first writing down a restore-plus-re-revoke procedure.
      timeToLiveAttribute: 'ExpiresAt',
    });
    // Listing a user's keys, and counting them for the 10-key cap.
    apiKeyTable.addGlobalSecondaryIndex({
      indexName: 'OwnerIndex',
      partitionKey: {
        name: 'OwnerId',
        type: AttributeType.STRING,
      },
      sortKey: {
        name: 'CreatedAt',
        type: AttributeType.NUMBER,
      },
      projectionType: ProjectionType.ALL,
    });

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
    const authorizerLogGroup = new LogGroup(this, 'authorizerLogGroup', logGroupDefaults);
    const manageKeysLogGroup = new LogGroup(this, 'manageKeysLogGroup', logGroupDefaults);

    // 3x Lambda
    const authorizerLambda = new RustFunction(this, 'authorizer', {
      manifestPath: 'lambda/authorizer/Cargo.toml',
      runtime: 'provided.al2023',
      architecture: Architecture.ARM_64,
      // Short: it does one JWKS fetch (cold only) or one GetItem. A long timeout here
      // would just hold a failing request open in front of every API call.
      timeout: cdk.Duration.seconds(10),
      logGroup: authorizerLogGroup,
      loggingFormat: LoggingFormat.JSON,
      environment: {
        COGNITO_POOL_ID: userPool.userPoolId,
        COGNITO_CLIENT_ID: userPoolClient.userPoolClientId,
        COGNITO_REGION: this.region,
        API_KEY_TABLE_NAME: apiKeyTable.tableName,
      }
    });
    // Verification is a hash lookup; LastUsedAt is a best-effort write, so it needs both.
    apiKeyTable.grantReadWriteData(authorizerLambda);

    const manageKeysLambda = new RustFunction(this, 'manageKeys', {
      manifestPath: 'lambda/manage_keys/Cargo.toml',
      runtime: 'provided.al2023',
      architecture: Architecture.ARM_64,
      timeout: cdk.Duration.seconds(30),
      logGroup: manageKeysLogGroup,
      loggingFormat: LoggingFormat.JSON,
      environment: {
        API_KEY_TABLE_NAME: apiKeyTable.tableName,
      }
    });
    apiKeyTable.grantReadWriteData(manageKeysLambda);

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
        // Was ['*'] with only content-type. Credentialed requests must not be readable by
        // any origin, and the two auth headers have to be allow-listed or the browser
        // strips them on preflight.
        allowHeaders: ['content-type', 'authorization', 'x-api-key'],
        allowMethods: [
          CorsHttpMethod.GET,
          CorsHttpMethod.POST,
          CorsHttpMethod.DELETE,
          CorsHttpMethod.OPTIONS,
        ],
        allowOrigins: [`https://${SITE_DOMAIN}`],
        allowCredentials: false,
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

    // Two authorizers, chosen per route. This split IS the FR-4.4 security boundary.
    //
    // /api/links must accept EITHER a Cognito JWT or an API key, which a native user pool
    // authorizer cannot do -- it rejects anything without a valid JWT. So that route gets
    // a custom Lambda authorizer that understands both.
    //
    // /api/keys gets the NATIVE pool authorizer precisely BECAUSE it only understands
    // JWTs: an API-key-only request never reaches the handler, so there is no code path
    // that could mistakenly let a leaked key mint its own replacements. Enforcing that at
    // the edge beats a conditional inside the handler, where a missing `if` fails open.
    const linksAuthorizer = new HttpLambdaAuthorizer('linksAuthorizer', authorizerLambda, {
      responseTypes: [HttpLambdaResponseType.SIMPLE],
      // Deliberately EMPTY, and it must stay empty. An HTTP API treats every declared
      // identity source as REQUIRED: if any one of them is absent from the request,
      // API Gateway returns 401 without ever invoking the authorizer. Listing both
      // Authorization and X-Api-Key therefore demanded BOTH credentials at once, which
      // no real client sends -- a browser (Authorization only) and a script (X-Api-Key
      // only) were both rejected at the edge, defeating the either/or design of FR-4.3.
      // With no identity sources API Gateway always invokes the authorizer, which reads
      // whichever header is present and decides for itself. Legal only because caching
      // is off below: identity sources double as the cache key, so AWS requires at
      // least one whenever resultsCacheTtl > 0.
      identitySource: [],
      // Zero cache is required, not an oversight. Any TTL is a window in which a revoked
      // key still authorizes requests, and FR-4.6 requires revocation to take effect on
      // the very next call. The cost is one extra Lambda invocation per API request.
      resultsCacheTtl: cdk.Duration.seconds(0),
    });

    const keysAuthorizer = new HttpUserPoolAuthorizer('keysAuthorizer', userPool, {
      userPoolClients: [userPoolClient],
    });

    // Integrations
    const createLinkInteg = new HttpLambdaIntegration('createLinkInteg', createLinkLambda);
    api.addRoutes({
      path: '/api/links',
      methods: [HttpMethod.POST],
      integration: createLinkInteg,
      authorizer: linksAuthorizer,
    });
    const getLinksInteg = new HttpLambdaIntegration('getLinksInteg', getLinksLambda);
    api.addRoutes({
      path: '/api/links',
      methods: [HttpMethod.GET],
      integration: getLinksInteg,
      authorizer: linksAuthorizer,
    });

    // Key management. JWT-only by construction (see above).
    const manageKeysInteg = new HttpLambdaIntegration('manageKeysInteg', manageKeysLambda);
    api.addRoutes({
      path: '/api/keys',
      methods: [HttpMethod.GET, HttpMethod.POST],
      integration: manageKeysInteg,
      authorizer: keysAuthorizer,
    });
    api.addRoutes({
      path: '/api/keys/{keyId}',
      methods: [HttpMethod.DELETE],
      integration: manageKeysInteg,
      authorizer: keysAuthorizer,
    });

    // Public redirect path -- deliberately NO authorizer. Ownership controls management,
    // not resolution: anyone holding a short URL can follow it (FR-2.3, FR-3.5).
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
        // OAuth callback. MUST exist and MUST precede the '/?*' catch-all below.
        //
        // '/?*' routes to API Gateway and '?' matches exactly one character, so
        // '/auth/callback' matches it and would be handed to visit_link as a short-link
        // lookup -- the same collision '/index.html' hit during the OAC migration.
        //
        // The object is extensionless (website/auth/callback), matching the existing
        // /terms and /privacy pattern, so the content-type must be forced: the OAC REST
        // origin serves it as binary otherwise and the browser downloads it.
        '/auth/*': {
          origin: s3Origin,
          viewerProtocolPolicy: ViewerProtocolPolicy.REDIRECT_TO_HTTPS,
          cachePolicy: CachePolicy.CACHING_OPTIMIZED,
          originRequestPolicy: OriginRequestPolicy.CORS_S3_ORIGIN,
          responseHeadersPolicy: new cdk.aws_cloudfront.ResponseHeadersPolicy(this, 'AuthResponseHeaders', {
            customHeadersBehavior: {
             customHeaders: [
               { header: 'content-type', value: 'text/html; charset=utf-8', override: true}
             ]
            }
          }),
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
      sources: [
        Source.asset('./website'),
        // Generated at deploy time so the pool and client IDs are never hardcoded in the
        // repo and never need a manual post-deploy edit. Source.data resolves the CDK
        // tokens during deployment and writes the file alongside the static assets,
        // overwriting the committed placeholder.
        Source.data(
          'assets/auth-config.js',
          [
            '// GENERATED AT DEPLOY TIME by lib/krtk-rs-stack.ts -- do not edit.',
            '// The committed version of this file is a placeholder with fake values so the',
            '// page still loads when opened locally.',
            'window.KRTK_AUTH = {',
            `  userPoolId: '${userPool.userPoolId}',`,
            `  clientId: '${userPoolClient.userPoolClientId}',`,
            `  authDomain: '${AUTH_DOMAIN}',`,
            `  region: '${this.region}',`,
            `  redirectUri: 'https://${SITE_DOMAIN}/auth/callback',`,
            `  logoutUri: 'https://${SITE_DOMAIN}/',`,
            '};',
            '',
          ].join('\n'),
        ),
      ],
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
