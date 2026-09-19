#!/usr/bin/env node
import * as cdk from 'aws-cdk-lib';
import { KrtkRsStack } from '../lib/krtk-rs-stack';
import { CertificateStack } from '../lib/certificate-stack';
import { SecretsStack } from '../lib/secrets-stack';
import { LogDeliveryStack } from '../lib/log-delivery-stack';

const app = new cdk.App();
const certStack = new CertificateStack(app, 'CertificateStack', {
  env: {
    account: '503716878456',
    region: 'us-east-1'
  },
  crossRegionReferences: true,
});
const secretsStack = new SecretsStack(app, 'SecretsStack', {
  env: {
    account: '503716878456',
    region: 'us-west-2'
  },
  crossRegionReferences: true,
});
const krtkStack = new KrtkRsStack(app, 'KrtkRsStack', {
  env: {
    account: '503716878456',
    region: 'us-west-2'
  },
  certificateArn: certStack.certificate.certificateArn,
  authCertificateArn: certStack.authCertificate.certificateArn,
  googleApiKeySecret: secretsStack.googleApiSecret,
  crossRegionReferences: true,
});

krtkStack.addDependency(certStack);
krtkStack.addDependency(secretsStack);

// The three v2 delivery constructs must be created in us-east-1 (a CloudWatch delivery
// API constraint), even though the log bucket lives in us-west-2 with the main stack.
// This consumes the distribution + bucket ARNs from KrtkRsStack via crossRegionReferences,
// so it depends on that stack (the reverse of the cert dependency).
const logDeliveryStack = new LogDeliveryStack(app, 'LogDeliveryStack', {
  env: {
    account: '503716878456',
    region: 'us-east-1',
  },
  crossRegionReferences: true,
  distributionArn: krtkStack.distributionArn,
  logBucketArn: krtkStack.logBucketArn,
});

logDeliveryStack.addDependency(krtkStack);
