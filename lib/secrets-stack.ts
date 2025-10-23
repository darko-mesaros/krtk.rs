import * as cdk from 'aws-cdk-lib';
import { Secret } from 'aws-cdk-lib/aws-secretsmanager';

export class SecretsStack extends cdk.Stack {
  public readonly googleApiSecret: Secret;
  constructor(scope: cdk.App, id: string, props?: cdk.StackProps) {
    super(scope, id, props);

    // Push key after creation
    this.googleApiSecret = new Secret(this, 'googleApiSecret',{
      description: 'Google API Key',
    });

    new cdk.CfnOutput(this, 'googleApiSecretArn',{
      value: this.googleApiSecret.secretArn,
      exportName: 'googleApiSecretArn'
    });
  }
}
