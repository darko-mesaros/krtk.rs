import * as cdk from 'aws-cdk-lib';
import { CfnDeliverySource, CfnDeliveryDestination, CfnDelivery } from 'aws-cdk-lib/aws-logs';

export interface LogDeliveryStackProps extends cdk.StackProps {
  /** Global-format CloudFront distribution ARN (the v2 delivery source). */
  distributionArn: string;
  /** ARN of the CloudFront access-log bucket (the v2 delivery destination). */
  logBucketArn: string;
}

/**
 * The three CloudWatch vended-log-delivery constructs that turn on CloudFront standard
 * access logging (v2, JSON) into the log bucket.
 *
 * This stack exists solely because the delivery API must be called in us-east-1, even
 * when the destination bucket is elsewhere. The bucket and the consuming Lambda stay in
 * the main us-west-2 stack (an S3 event and its target Lambda must share a region); only
 * these three constructs live here, wired via crossRegionReferences. It mirrors the
 * CertificateStack precedent (a small us-east-1 stack feeding the main us-west-2 stack).
 *
 * See design.md section 2.
 */
export class LogDeliveryStack extends cdk.Stack {
  constructor(scope: cdk.App, id: string, props: LogDeliveryStackProps) {
    super(scope, id, props);

    // The distribution is the log source.
    const deliverySource = new CfnDeliverySource(this, 'cfAccessLogSource', {
      name: 'krtk-cf-access-logs',
      logType: 'ACCESS_LOGS',
      resourceArn: props.distributionArn,
    });

    // The log bucket is the destination. outputFormat is fixed at destination-create
    // time and cannot be changed later; JSON is chosen deliberately so the parser
    // addresses fields by name rather than column position.
    const deliveryDestination = new CfnDeliveryDestination(this, 'cfAccessLogDestination', {
      name: 'krtk-cf-log-bucket',
      destinationResourceArn: props.logBucketArn,
      outputFormat: 'json',
    });

    // Links source to destination and selects the minimal field set. The two carried
    // fields (timestamp(ms), c-country) are free and avoid a delivery reconfigure if the
    // deferred per-link analytics are ever built.
    //
    // fieldDelimiter is deliberately omitted: it is irrelevant for JSON output, and
    // CloudFormation rejects an empty-string delimiter (minimum length 1), so passing ''
    // as the spec suggested fails synth. Leaving it unset is the correct JSON-output form.
    const delivery = new CfnDelivery(this, 'cfAccessLogDelivery', {
      deliverySourceName: deliverySource.name,
      deliveryDestinationArn: deliveryDestination.attrArn,
      recordFields: ['cs-method', 'sc-status', 'cs-uri-stem', 'timestamp(ms)', 'c-country'],
    });
    // The delivery references both by name/ARN, so make the ordering explicit rather than
    // relying on synthesis order.
    delivery.node.addDependency(deliverySource);
    delivery.node.addDependency(deliveryDestination);
  }
}
