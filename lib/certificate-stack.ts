import * as cdk from 'aws-cdk-lib';
import { HostedZone, ARecord, RecordTarget, CnameRecord } from 'aws-cdk-lib/aws-route53';
import { Certificate, CertificateValidation} from 'aws-cdk-lib/aws-certificatemanager';

export class CertificateStack extends cdk.Stack {
  public readonly certificate: Certificate;
  constructor(scope: cdk.App, id: string, props?: cdk.StackProps) {
    super(scope, id, props);

    // Route53
    const hostedZone = HostedZone.fromHostedZoneAttributes(this, 'hostedZone',{
      zoneName: 'krtk.rs',
      hostedZoneId: 'Z07540833AST0TH4M5W39',
    })

    // Domain
    this.certificate = new Certificate(this, 'cert',{
      domainName: 'krtk.rs',
      validation: CertificateValidation.fromDns(hostedZone),
    });

    new cdk.CfnOutput(this, 'CertificateArn',{
      value: this.certificate.certificateArn,
      exportName: 'CertificateArn'
    });
  }
}
