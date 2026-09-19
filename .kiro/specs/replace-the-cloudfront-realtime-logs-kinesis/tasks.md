# Tasks: Replace CloudFront realtime-logs > Kinesis analytics with S3 access logs

Topology: **option B** (log bucket + analytics Lambda + S3 event in `KrtkRsStack`/us-west-2;
the three v2 delivery constructs in a new `LogDeliveryStack`/us-east-1). Do not merge any PR;
drive to review-ready and green, then hand off. Use `>` not `->` and no em dashes in prose.

Before any `npm test` / `cdk synth` / `cdk diff`:
`export PATH="$HOME/.cargo/bin:$PATH"` (else cargo-lambda-cdk falls back to absent Docker and
every CDK test fails with `FailedToBundleAsset ... docker ENOENT`).

Work in a worktree/branch off `main`. Suggested branch: `feat/s3-access-log-analytics`.

---

## Phase 1: Rust analytics rewrite (no AWS calls needed to test the core)

- [x] **1.1 Cargo dependencies.**
  - In `Cargo.toml` `[workspace.dependencies]`: change `aws_lambda_events` features from
    `["kinesis"]` to `["s3"]`.
  - Add to `[workspace.dependencies]`:
    `aws-sdk-s3 = { version = "1", default-features = false, features = ["default-https-client", "rt-tokio"] }`
    and `flate2 = "1"`.
  - In `lambda/process_analytics/Cargo.toml`: depend on `aws-sdk-s3`, `flate2`, and the `s3`
    feature of `aws_lambda_events` (via workspace); drop any `kinesis`-specific usage.

- [x] **1.2 Pure parser module** in `lambda/process_analytics/src/`.
  - `LogRecord`: `#[derive(Deserialize)]` with `#[serde(rename = "...")]` mapping
    `sc-status`, `cs-uri-stem`, `cs-method` (required) and `timestamp(ms)`, `c-country`
    (carried, unused this change) to Rust fields.
  - `fn classify(record: &LogRecord) -> Option<String>`: returns `Some(link_id)` only when
    `cs-method == "GET"` AND `sc-status == "302"` AND `cs-uri-stem` matches `^/[^/]+$` AND is
    not `/index.html`, `/terms`, `/privacy`, or `/`, AND does not start with `/api/`,
    `/assets/`, `/auth/`. `link_id` is `cs-uri-stem` without the leading `/`.
  - `fn parse_log_object(decompressed: &str) -> Vec<String>`: split into lines, deserialize
    each line as `LogRecord`, skip lines that fail to deserialize (header `#Fields:`, blank,
    truncated) without panicking, run `classify`, collect the `Some` link ids.
  - `fn aggregate(link_ids: &[String]) -> HashMap<String, u64>`.

- [x] **1.3 Parser unit tests** (in the same crate, `#[cfg(test)]`, no AWS):
  valid short-link 302 GET > Some; `/api/links` > None; `/assets/main.js` > None;
  `/auth/callback` > None; `/index.html` > None; `/` > None; 404 on a short path > None;
  200 on a short path > None; HEAD 302 > None; malformed/truncated line and `#Fields:` header
  > skipped, no panic; `aggregate` of 50 hits on one link + 3 on another > `{a:50, b:3}`.

- [x] **1.4 Shared increment-by method.** In `shared/src/core.rs`, add
  `pub async fn increment_click_count_by(&self, short_url: &str, by: u64) -> Result<(), AppError>`:
  same as `increment_click_count` but `:val` = `by`. Keep `SET Clicks = Clicks + :val`,
  `attribute_exists(LinkId)`, `ReturnValue::AllNew`, and the `AppError::database` mapping
  (renders "Data store operation failed", names no backend service). Leave
  `increment_click_count` in place.

- [x] **1.5 Idempotency marker** in `shared/src/core.rs` (or the analytics crate, wherever the
  DynamoDB client lives). Add
  `pub async fn claim_log_object(&self, object_key: &str) -> Result<bool, AppError>`:
  `PutItem` `LinkId = format!("PROCESSED#{object_key}")`, a TTL attribute ~30 days out,
  condition `attribute_not_exists(LinkId)`. Return `Ok(true)` on success, `Ok(false)` on
  `ConditionalCheckFailedException`, `Err` otherwise. Write NO `SortKey` attribute, so the row
  never enters `TimeStampIndex`.
  - Unit test the "already claimed > false > zero increments" branch (FR-12.4).

- [x] **1.6 S3 handler rewrite** in `lambda/process_analytics/src/main.rs`.
  - Consume `aws_lambda_events::event::s3::S3Event`. Build `aws-sdk-s3` and `aws-sdk-dynamodb`
    clients in `main`, keep `TABLE_NAME` / `SHORTENER_DOMAIN` env vars.
  - Per record: extract bucket+key; `claim_log_object(key)` first (if `false`, log `info`,
    skip object); `GetObject`; read body; `flate2::read::GzDecoder` to a `String`;
    `parse_log_object`; `aggregate`; one `increment_click_count_by(link, count)` per distinct
    link. On a per-link error, `tracing::warn!(...)` (feeds the alarm) and continue.
  - Remove the old Kinesis loop, the `CfAnalyticsData` struct, the tab-split, and the
    `fields[3]` positional indexing entirely. No `.expect()`/`panic!` on log content.

- [x] **1.7 `cargo test`** for the workspace passes (parser + idempotency branch + existing
  `error.rs` tests). `cargo build` clean.

## Phase 2: CDK infrastructure (topology B)

- [x] **2.1 Log bucket** in `lib/krtk-rs-stack.ts` (us-west-2). New `Bucket cfLogBucket`:
  `removalPolicy DESTROY`, `autoDeleteObjects true`, `blockPublicAccess BLOCK_ALL`,
  `enforceSSL true`, `objectOwnership ObjectOwnership.BUCKET_OWNER_ENFORCED` (import from
  `aws-cdk-lib/aws-s3`), `lifecycleRules: [{ expiration: cdk.Duration.days(30) }]`, no
  versioning. Do not reuse `hostingBucket`.

- [x] **2.2 Delivery bucket policy** on `cfLogBucket`: `addToResourcePolicy` an `Allow` for
  service principal `delivery.logs.amazonaws.com`, action `s3:PutObject`, resource the log
  prefix `/*`, conditions `StringEquals { 'aws:SourceAccount': this.account }` and
  `StringEquals { 's3:x-amz-acl': 'bucket-owner-full-control' }`.

- [x] **2.3 Rewire `processAnalyticsLambda`** (keep the function, log group, alarm).
  - Remove `cfAnalyticsStream.grantRead(...)`, the `KinesisEventSource` registration, the
    `Stream`, and `RealtimeLogConfig`.
  - Add `cfLogBucket.grantRead(processAnalyticsLambda)`; keep
    `linkDatabase.grantWriteData(processAnalyticsLambda)` (also covers the marker PutItem,
    same table).
  - Add `processAnalyticsLambda.addEventSource(new S3EventSource(cfLogBucket, { events:
    [EventType.OBJECT_CREATED], filters: [{ prefix: '<log prefix>' }] }))` from
    `aws-cdk-lib/aws-lambda-event-sources` (import `EventType` from `aws-cdk-lib/aws-s3`).

- [x] **2.4 Remove Kinesis from the distribution.** Delete `realtimeLogConfig: realTimeConfig`
    from the `/?*` behaviour; leave that behaviour otherwise identical.

- [x] **2.5 Clean up imports** in `lib/krtk-rs-stack.ts`: drop `Endpoint`, `RealtimeLogConfig`,
    `Stream`, `StreamMode`, `KinesisEventSource`, and `StartingPosition` (verify no other use).

- [x] **2.6 New `lib/log-delivery-stack.ts`** (us-east-1). Props: `distributionArn: string`,
    `logBucketArn: string`. Declares:
  - `CfnDeliverySource` (`aws-cdk-lib/aws-logs`): `logType 'ACCESS_LOGS'`,
    `resourceArn: props.distributionArn`, a stable `name`.
  - `CfnDeliveryDestination`: `destinationResourceArn: props.logBucketArn`,
    `outputFormat: 'json'`, a stable `name`.
  - `CfnDelivery`: `deliverySourceName` = the source's name, `deliveryDestinationArn` =
    destination `attrArn`, `recordFields: ['cs-method','sc-status','cs-uri-stem','timestamp(ms)','c-country']`,
    `fieldDelimiter: ''`.

- [x] **2.7 Wire the new stack in `bin/krtk-rs.ts`.** Instantiate `KrtkRsStack` first, then
    `new LogDeliveryStack(app, 'LogDeliveryStack', { env: { account: '503716878456', region:
    'us-east-1' }, crossRegionReferences: true, distributionArn: krtkStack.distributionArn,
    logBucketArn: krtkStack.logBucketArn })`. Add `logDeliveryStack.addDependency(krtkStack)`
    (reverse of the cert dependency: this stack consumes the distribution ARN). Expose
    `distributionArn` and `logBucketArn` as public readonly fields on `KrtkRsStack` (the
    distribution ARN is global-format `arn:aws:cloudfront::<account>:distribution/<id>`;
    derive it from `cdn.distributionId` via `cdk.Stack.of(this).formatArn` or
    `` `arn:${this.partition}:cloudfront::${this.account}:distribution/${cdn.distributionId}` ``).

## Phase 3: Tests

- [x] **3.1 Revise `test/krtk-rs.test.ts`** (revise, do not delete wholesale):
  - Replace the `Kinesis analytics stream` block with `resourceCountIs('AWS::Kinesis::Stream', 0)`.
  - Replace `processAnalytics is wired to the Kinesis stream ...` with
    `resourceCountIs('AWS::Lambda::EventSourceMapping', 0)` and an assertion that the log
    bucket carries an S3 `NotificationConfiguration` targeting a Lambda (whatever synth emits:
    `AWS::S3::Bucket` `NotificationConfiguration` or the CDK notifications custom resource).
  - Replace `attaches the realtime log config ...` with
    `resourceCountIs('AWS::CloudFront::RealtimeLogConfig', 0)` and assert the `/?*` behaviour
    has no `RealtimeLogConfigArn`.
  - Add: log bucket has `BLOCK_ALL`, `enforceSSL` deny-non-TLS, `BUCKET_OWNER_ENFORCED`
    (`OwnershipControls` = `BucketOwnerEnforced`), a lifecycle `ExpirationInDays: 30`, and
    `DeletionPolicy: Delete`.
  - Add: log bucket policy allows `delivery.logs.amazonaws.com` `s3:PutObject` with the
    `aws:SourceAccount` condition; the existing "never an Allow to a wildcard principal" guard
    still holds for the new policy.
  - Keep the six-functions / arm64 / JSON-logging / log-group tests (process_analytics is still
    one of the six), the alarm test (threshold still 10), and all DynamoDB/CloudFront/Cognito
    tests.

- [x] **3.2 New `LogDeliveryStack` test.** Synth it in us-east-1 with stub
    `distributionArn`/`logBucketArn` and assert one `AWS::Logs::DeliverySource` (`LogType
    ACCESS_LOGS`), one `AWS::Logs::DeliveryDestination` (`OutputFormat json`), one
    `AWS::Logs::Delivery` with the expected `RecordFields`.

- [x] **3.3 Run the suites** (with `PATH` set): `npm test` green, `cargo test` green.

- [x] **3.4 `cdk synth`** succeeds; grep the synthesized template(s) to confirm zero
    `AWS::Kinesis::Stream` and zero `AWS::CloudFront::RealtimeLogConfig`.

## Phase 4: Documentation

- [x] **4.1 README Data Flow (section 2):** replace the two Kinesis lines with the S3 pipeline
    (CloudFront writes a standard access-log file to the log bucket; an S3 ObjectCreated event
    invokes process_analytics, which filters to short-link 302s and increments Clicks).
    Document the seconds-to-minutes latency as an expected property, not a regression.

- [x] **4.2 README Infrastructure list:** remove the Kinesis bullet; change the
    `processAnalyticsLambda` line from "Handles the CF access logs from kinesis" to "consumes
    CloudFront S3 access logs on ObjectCreated and increments click counts"; add `cfLogBucket`
    (CloudFront access logs, 30-day expiry) under S3.

- [x] **4.3 README ASCII diagram:** replace the `[Kinesis]` node with `[S3 access logs]`
    feeding `[Lambda]` on ObjectCreated.

## Phase 5: Deploy and verify (human-gated; do not merge)

- [ ] **5.1 Deploy** `cdk deploy --all --profile default` (note the new us-east-1
    `LogDeliveryStack`). Watch for the delivery-source "already exists" error (only one
    delivery source per distribution); if a prior manual/legacy delivery exists, delete it
    first.

- [ ] **5.2 Console check:** distribution Logging tab shows the v2 S3 delivery Enabled.

- [ ] **5.3 Sparse-GSI check:** confirm a `PROCESSED#...` marker row does NOT appear in
    `list_urls` for the admin user (proves markers stay out of `TimeStampIndex`).

- [ ] **5.4 Functional check:** shorten a fresh link, visit it 3 times, wait for delivery
    (usually minutes, up to ~an hour worst case), confirm `Clicks` incremented by exactly 3.
    Record the observed latency in minutes in the PR description.

- [ ] **5.5 Idempotency check:** re-process the same object (or observe a natural redelivery)
    and confirm `Clicks` did not change again.

- [ ] **5.6 No-inflation check:** confirm asset/API traffic during the window did not increment
    any counter.

- [ ] **5.7 Open the PR**, CI green, report URL + status. Hand off; do not merge.

---

## Acceptance criteria (from requirements.md)

- `cdk synth` succeeds; template has zero `AWS::Kinesis::Stream` and zero
  `AWS::CloudFront::RealtimeLogConfig` (3.4).
- `npm test` passes with revised assertions (3.1-3.3).
- `cargo test` passes incl. parser tests covering valid 302, /api/, /assets/, 404, malformed
  (1.3).
- Idempotency guard test proves same object twice increments once (1.5).
- Post-deploy verification plan executed and latency recorded (Phase 5).
