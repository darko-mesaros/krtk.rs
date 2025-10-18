#!/usr/bin/env node
import * as cdk from 'aws-cdk-lib';
import { KrtkRsStack } from '../lib/krtk-rs-stack';
import { CertificateStack } from '../lib/certificate-stack';
import { SecretsStack } from '../lib/secrets-stack';

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
  googleApiKeySecret: secretsStack.googleApiSecret,
  crossRegionReferences: true,
});

krtkStack.addDependency(certStack);
krtkStack.addDependency(secretsStack);
