import * as cdk from 'aws-cdk-lib';
import { HostedZone, ARecord, RecordTarget, CnameRecord } from 'aws-cdk-lib/aws-route53';
import { Certificate, CertificateValidation} from 'aws-cdk-lib/aws-certificatemanager';

export class CertificateStack extends cdk.Stack {
  public readonly certificate: Certificate;
  /**
   * Certificate for the Cognito Hosted UI at auth.krtk.rs.
   *
   * Kept separate from the site certificate rather than added as a subject
   * alternative name: Cognito requires a certificate whose domain matches the
   * custom domain exactly, and a SAN-bundled cert would couple the login page's
   * lifecycle to the site's.
   *
   * This stack is already pinned to us-east-1, which is where Cognito requires
   * the certificate to live *regardless* of the user pool's own region — the pool
   * itself is in us-west-2 with the rest of the application.
   */
  public readonly authCertificate: Certificate;

  constructor(scope: cdk.App, id: string, props?: cdk.StackProps) {
    super(scope, id, props);

    // Route53
    // TODO: Remove the hardcoded domain and HostedZoneId
    const hostedZone = HostedZone.fromHostedZoneAttributes(this, 'hostedZone',{
      zoneName: 'krtk.rs', 
      hostedZoneId: 'Z07540833AST0TH4M5W39',
    })

    // Domain
    this.certificate = new Certificate(this, 'cert',{
      domainName: 'krtk.rs',
      validation: CertificateValidation.fromDns(hostedZone),
    });

    // Hosted UI domain. DNS-validated against the same zone, so issuance needs no
    // manual step.
    this.authCertificate = new Certificate(this, 'authCert', {
      domainName: 'auth.krtk.rs',
      validation: CertificateValidation.fromDns(hostedZone),
    });

    new cdk.CfnOutput(this, 'CertificateArn',{
      value: this.certificate.certificateArn,
      exportName: 'CertificateArn'
    });

    new cdk.CfnOutput(this, 'AuthCertificateArn', {
      value: this.authCertificate.certificateArn,
      exportName: 'AuthCertificateArn',
      description: 'ACM cert for the Cognito Hosted UI at auth.krtk.rs (must be us-east-1)',
    });
  }
}
