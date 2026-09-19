# Design: Replace CloudFront realtime-logs > Kinesis analytics with S3 access logs

## Overview

Replace the CloudFront realtime-logs > Kinesis > `process_analytics` pipeline with CloudFront
standard access logging (v2) delivered to a dedicated S3 bucket, consumed by a rewritten
`process_analytics` Lambda triggered on `s3:ObjectCreated`. The Lambda fetches and gunzips
the JSON log object, parses records by field name, filters to short-link 302s, aggregates
hits per link in memory, applies one `UpdateItem` per distinct link, and guards against
double-counting with a per-object marker in DynamoDB.

This is a like-for-like replacement of click counting. The redirect path, the `Clicks`
attribute, the `/api/links` wire format, and the `linkTable` schema are all unchanged. Richer
analytics are deferred (FR-11).

The mechanism decision is settled: **standard logging v2** (CloudWatch vended log delivery),
JSON output, minimal field set. See §2 for why and for the us-east-1 wrinkle it introduces.

## 1. What is removed

From `lib/krtk-rs-stack.ts`:

- `cfAnalyticsStream` (`new Stream(...)`, line 79).
- `realTimeConfig` (`new RealtimeLogConfig(...)`, line 85).
- `realtimeLogConfig: realTimeConfig` on the `/?*` behaviour (line 602). The rest of that
  behaviour (origin, `CACHING_DISABLED`, `ALL_VIEWER_EXCEPT_HOST_HEADER`, `ALLOW_ALL`) stays.
- `cfAnalyticsStream.grantRead(processAnalyticsLambda)` and the `KinesisEventSource`
  registration (lines 383-389).
- Now-unused imports: `Endpoint`, `RealtimeLogConfig` (line 17), `Stream`, `StreamMode`
  (line 19), `KinesisEventSource` (line 20), and `StartingPosition` from the
  `aws-cdk-lib/aws-lambda` import (line 21) if nothing else uses it (nothing does).

The `processAnalyticsLambda` function, its `processAnalyticsLogGroup`, the
`invalidUrlMetricFilter`, and the `invalidUrlAlarm` are KEPT and rewired (§3, §6).

## 2. Standard logging v2, and the us-east-1 constraint

### 2.1 Why v2 over v1

v1 (`enableLogging: true` + `logBucket` on the Distribution L2) is three lines, but it emits a
fixed 33-field gzipped TSV with a two-line header, which puts the parser back on column
positions -- exactly the fragility that makes the current Kinesis parser panic on drift
(FR-6). v1 also requires S3 ACLs enabled on the log bucket
(`ObjectOwnership: BUCKET_OWNER_PREFERRED`), which sits badly against this project's
`BLOCK_ALL` + `enforceSSL` posture.

v2 uses CloudWatch vended-log delivery, lets us select only the fields we need, emits JSON
(field-name addressable, satisfies FR-6), and uses a bucket policy rather than ACLs. It is
more plumbing in CDK because there is no L2 -- three L1 constructs plus a bucket policy -- but
that plumbing buys a robust parser and keeps the bucket ACL-free.

Verified against aws-cdk-lib 2.264.0: `CfnDeliverySource`, `CfnDeliveryDestination`, and
`CfnDelivery` exist in `aws-cdk-lib/aws-logs` (present since 2.262.x). No escape hatch beyond
these L1 constructs is needed, so the v1 fallback is not triggered.

### 2.2 The us-east-1 constraint (decision embedded here, flagged for review)

The CloudWatch delivery API that provisions v2 delivery MUST be called in `us-east-1`, even
when the destination bucket is elsewhere (AWS documents this explicitly). The main
`KrtkRsStack` runs in `us-west-2`. So the three delivery constructs cannot simply be declared
inline in the main stack and target `us-west-2`.

**Options considered:**

- **A. Put the delivery constructs (and the log bucket) in a new small us-east-1 stack**,
  analogous to `CertificateStack`, and pass the distribution ARN across via
  `crossRegionReferences: true` (already enabled for the cert/secret wiring in
  `bin/krtk-rs.ts`). The `process_analytics` Lambda and its S3 event source stay in the main
  us-west-2 stack, subscribing to the us-east-1 bucket's events.
- **B. Put the log bucket in us-west-2 (main stack) but the delivery constructs in a
  us-east-1 stack.** S3 delivery destinations can point cross-region, and a bucket's
  `ObjectCreated` events fire in the bucket's own region where the Lambda also lives.

**Recommendation: B.** Keeping the log bucket in us-west-2 alongside the consuming Lambda
keeps the S3-event > Lambda wiring entirely in-region and in one stack (no cross-region event
source, which S3 does not support anyway -- an S3 event notification and its target Lambda
must share a region). Only the three delivery constructs live in a new us-east-1 stack
(`LogDeliveryStack`), which references the distribution ARN and the bucket ARN via
`crossRegionReferences`. The bucket policy granting `delivery.logs.amazonaws.com` write access
is attached to the bucket in the main stack.

This adds one new stack. It mirrors the existing `CertificateStack` precedent exactly (a small
us-east-1 stack feeding the main us-west-2 stack), so it is consistent, not novel.

> DECISION FOR REVIEW: adopt option B (log bucket in us-west-2 main stack, delivery constructs
> in a new us-east-1 `LogDeliveryStack`). If you would rather not add a stack, option A puts
> the bucket in us-east-1 too, but then the S3-event Lambda must also move to us-east-1,
> splitting the analytics function away from the rest of the app. B is the smaller blast
> radius.

### 2.3 Delivery wiring (option B)

New `lib/log-delivery-stack.ts`, region us-east-1:

- `CfnDeliverySource`: `name: 'krtk-cf-access-logs'`, `logType: 'ACCESS_LOGS'`,
  `resourceArn: <distribution ARN>` (passed in as a prop from the main stack).
- `CfnDeliveryDestination`: `name: 'krtk-cf-log-bucket'`,
  `destinationResourceArn: <log bucket ARN>`, `outputFormat: 'json'`.
  (Output format is fixed at destination-create time and cannot be changed later; JSON is
  chosen deliberately.)
- `CfnDelivery`: links source to destination. `recordFields` = the minimal set (§4.1).
  `fieldDelimiter` is irrelevant for JSON output but set to `''` to match the API default.

Main stack (`us-west-2`) additions:

- The log bucket (§5).
- A bucket policy statement allowing `delivery.logs.amazonaws.com` `s3:PutObject` on the
  bucket's log prefix, conditioned on `aws:SourceAccount` = this account and
  `s3:x-amz-acl = bucket-owner-full-control`. (CDK/CFN does not auto-add this the way the
  console does, so it is explicit.)
- The distribution ARN is exported for the delivery stack. CloudFront distribution ARNs are
  global (`arn:aws:cloudfront::<account>:distribution/<id>`), so no region mismatch.

## 3. The rewritten `process_analytics` Lambda

### 3.1 Trigger and shape

- CDK: `processAnalyticsLambda.addEventSource(new S3EventSource(logBucket, { events:
  [EventType.OBJECT_CREATED], filters: [{ prefix: '<log prefix>' }] }))` from
  `aws-cdk-lib/aws-lambda-event-sources`. Grants: `logBucket.grantRead(processAnalyticsLambda)`
  and the existing `linkDatabase.grantWriteData(processAnalyticsLambda)`, plus the marker
  grant (§7). Function config unchanged: RustFunction, ARM_64, provided.al2023, JSON logging,
  explicit log group from `logGroupDefaults`, 30s timeout.
- Rust: handler consumes `aws_lambda_events::event::s3::S3Event`. `Cargo.toml` swaps the
  `aws_lambda_events` feature from `["kinesis"]` to `["s3"]`.
- New dependency for the S3 object fetch: `aws-sdk-s3` (same
  `default-features = false, features = ["default-https-client", "rt-tokio"]` pattern as the
  DynamoDB SDK, to keep the modern rustls stack). New dependency for gunzip: `flate2`
  (`GzDecoder`). Both added to `[workspace.dependencies]` and the crate's `Cargo.toml`.

### 3.2 Flow per S3 event record

For each record in the `S3Event`:

1. Extract bucket + object key.
2. Idempotency guard (§7): attempt to claim the object key. If already claimed, skip the whole
   object and log at `info`.
3. `GetObject`, read the body, `GzDecoder` to a `String` (CloudFront S3 log objects are
   gzipped).
4. Parse the JSON lines into records addressed by field name (§4).
5. Filter to short-link 302s (§4.2).
6. Aggregate into a `HashMap<String, u64>` of `link_id -> hit count`.
7. For each distinct link, one `increment_click_count_by(link_id, count)` call (§4.3).
8. On any per-link increment error, log at `warn` (feeds the alarm, FR-9) and continue; do not
   fail the whole invocation over one bad link.

### 3.3 Decoupling (FR-3)

Nothing here touches `visit_link` or the redirect behaviour. A total failure of this function
leaves redirects working; the only visible effect is stale click counts.

## 4. The parser

### 4.1 Fields selected in `recordFields`

Minimal set for click counting, plus a couple that are free to carry for the deferred
analytics (FR-11.2), noted but not consumed:

- `sc-status` -- required, the 302 filter.
- `cs-uri-stem` -- required, the path to classify and extract the link id.
- `cs-method` -- required, to count only GETs (a HEAD or OPTIONS to a short link is not a
  click).
- `timestamp(ms)`, `c-country` -- carried for future per-link analytics; deserialized but
  ignored by this change. Selecting them now costs nothing and avoids a delivery reconfigure
  later.

Deferred aggregates (country/referer/user-agent) would, given the no-sort-key constraint on
`linkTable` (FR-11.3), live either in synthetic `LinkId` partitions in the same table (for
example `LinkId = "AGG#<link>#country#<cc>"`, which the sparse `SortKey` GSI would keep out of
`list_urls`) or in a new dedicated table. This design does not build either; it only records
where they would go.

### 4.2 Classification: what counts as a short-link hit

A record is a countable click IF and ONLY IF all hold:

- `cs-method == "GET"`,
- `sc-status == "302"`,
- `cs-uri-stem` is a single path segment that is a plausible link id: it matches `^/[^/]+$`
  AND is not one of the reserved exact paths `/index.html`, `/terms`, `/privacy`, `/`
  (the bare root), AND does not begin with `/api/`, `/assets/`, or `/auth/`.

Because standard logs cover the ENTIRE distribution (not just `/?*`), this filter is
load-bearing (FR-4.4). The link id is `cs-uri-stem` with the leading `/` stripped. Any query
string is in a separate `cs-uri-query` field and is not part of `cs-uri-stem`, so no `?`
splitting is needed. The current code's `trim_start_matches("/")` plus tab-splitting is
replaced entirely.

### 4.3 Pure function, testable (FR-6.3)

```
// pure, no AWS
fn parse_log_object(decompressed: &str) -> Vec<ClickHit>
fn classify(record: &LogRecord) -> Option<String>   // Some(link_id) if countable, else None
fn aggregate(hits: &[ClickHit]) -> HashMap<String, u64>
```

`LogRecord` is a `#[derive(Deserialize)]` struct with `#[serde(rename = "...")]` on each field
so the JSON field names (`sc-status`, `cs-uri-stem`, `cs-method`) map to Rust field names.
Each log line is one JSON object; parse line by line, and on a serde error for a line, skip it
(FR-6.2) rather than failing the object. A `#Fields:` header line or blank line simply fails
to deserialize and is skipped.

### 4.4 New shared method

Add to `shared/src/core.rs` alongside `increment_click_count`:

```
pub async fn increment_click_count_by(&self, short_url: &str, by: u64) -> Result<(), AppError>
```

Identical to `increment_click_count` but `:val` is `by` instead of `"1"`, keeping the
`SET Clicks = Clicks + :val`, `attribute_exists(LinkId)` condition, and `ALL_NEW` return. The
existing `increment_click_count` is kept (it is still the natural unit-of-one API and its call
sites/tests remain valid) or expressed as `increment_click_count_by(url, 1)`; either is fine,
the tests pin behaviour not structure. Errors continue to map through `AppError::database`,
which renders "Data store operation failed" and names no backend service (FR-10.3).

## 5. The log bucket (FR-8)

New bucket in the main us-west-2 stack, separate from `hostingBucket`:

```
const logBucket = new Bucket(this, 'cfLogBucket', {
  removalPolicy: cdk.RemovalPolicy.DESTROY,
  autoDeleteObjects: true,
  blockPublicAccess: BlockPublicAccess.BLOCK_ALL,
  enforceSSL: true,
  objectOwnership: ObjectOwnership.BUCKET_OWNER_ENFORCED, // no ACLs; v2 uses a bucket policy
  lifecycleRules: [{ expiration: cdk.Duration.days(30) }],
});
```

- **Retention 30 days.** Rationale: counts are applied within minutes, so raw logs have no
  operational value past that. 30 days leaves a comfortable window to re-derive counts or
  debug a parser bug (re-process objects) without keeping logs indefinitely. Logs are
  disposable telemetry, so `DESTROY` + `autoDeleteObjects` matches the `hostingBucket`
  precedent (FR-8.4). No versioning (unlike `hostingBucket`): versioned log objects would
  defeat lifecycle expiry.
- `BUCKET_OWNER_ENFORCED` disables ACLs entirely, which is what lets v2 work without the ACL
  relaxation v1 would have forced. The delivery service writes via the bucket policy.
- Bucket policy (added in main stack): `Allow` `delivery.logs.amazonaws.com` `s3:PutObject` on
  `arn:.../<prefix>/*`, `Condition` `StringEquals aws:SourceAccount = <account>` and
  `s3:x-amz-acl = bucket-owner-full-control`. `enforceSSL` adds the usual `Deny` non-TLS
  statement; a wildcard principal on that Deny is fine (existing test already distinguishes
  Deny-wildcard from Allow-wildcard).

## 6. Preserving the invalid-URL alarm (FR-9)

The `invalidUrlMetricFilter` and `invalidUrlAlarm` stay bound to `processAnalyticsLogGroup`,
which is unchanged. The rewritten Lambda still emits `warn` on a failed increment, so the
`$.level = warn` filter keeps working.

**Threshold review.** Today failures arrive one Kinesis record per invocation (batchSize 1),
so 10 warns in 5 minutes meant roughly 10 bad hits. Under S3 batching, one invocation
processes a whole file and can emit many warns at once, so a single bad deploy could trip 10
warns from one file. That is arguably the point -- the alarm should fire when increments are
failing -- and at ~1,000 redirects/day across a handful of files per hour, 10 warns in 5
minutes is still a meaningful "something is wrong" signal, not routine noise. **Keep the
threshold at 10 in 5 minutes.** If it proves chatty post-deploy, raise it; do not lower it.
The design does not change it now.

## 7. Idempotency (FR-5)

S3 `ObjectCreated` is at-least-once; a single log object can be redelivered, and a redelivered
file is a far bigger blast radius than the old batchSize-1 Kinesis record. Chosen strategy:

**Conditional-PutItem marker keyed on the S3 object key, claimed BEFORE processing.**

- A marker row is written to `linkTable` (same table, no new table needed) with
  `LinkId = "PROCESSED#<object-key>"` and a `PutItem` conditioned on
  `attribute_not_exists(LinkId)`, plus a TTL attribute so markers self-expire (align with the
  log bucket's 30-day retention; a marker is useless once its object is gone).
- If the conditional `PutItem` succeeds, this invocation owns the object: proceed to count.
- If it fails with `ConditionalCheckFailedException`, the object was already claimed: skip
  entirely, log at `info`, return `Ok`.

**Same-table safety.** Marker rows carry no `SortKey` attribute, so they never enter the
`TimeStampIndex` GSI (its partition key is `SortKey`) and therefore cannot appear in
`list_urls`. This is the "GSI is sparse on SortKey" property. It must be verified before
relying on it (§9, verification), but DynamoDB's documented behaviour is that an item missing
the GSI's partition-key attribute is simply not indexed. The `PROCESSED#` prefix also keeps
markers from ever colliding with a real cuid2 link id (7 lowercase alphanumerics, no `#`).

**Claim-before-process means we LOSE, not inflate, on a mid-file crash (FR-5.2, FR-5.3).** If
the function claims the object, increments some links, then crashes before finishing, the
marker already exists, so the retry sees the claim and skips -- the un-applied increments are
lost. That is the correct trade under the requirement: under-counting is acceptable,
double-counting is not. The alternative (claim after processing) would re-run the whole file
on any mid-file crash and double-count everything already applied, which is exactly what we
must avoid.

Accepted residual: a rare crash between claim and completion loses part of one file's counts.
At ~1,000 clicks/day this is negligible and self-limiting to a single file.

## 8. Tests

### 8.1 CDK (`test/krtk-rs.test.ts`)

Revise, do not delete wholesale:

- **Remove/replace** the `Kinesis analytics stream` describe block (lines ~541-552): assert
  `resourceCountIs('AWS::Kinesis::Stream', 0)`.
- **Remove/replace** `processAnalytics is wired to the Kinesis stream via an event source
  mapping` (line ~135): the S3 event source is a bucket notification, not an
  `AWS::Lambda::EventSourceMapping`. Assert instead that the log bucket has a notification
  configuration targeting the function (via `AWS::S3::Bucket` `NotificationConfiguration` or
  the CDK custom resource, whichever synth produces), and that
  `resourceCountIs('AWS::Lambda::EventSourceMapping', 0)`.
- **Remove/replace** `attaches the realtime log config to the link-redirect behaviour`
  (lines ~333-337): assert `resourceCountIs('AWS::CloudFront::RealtimeLogConfig', 0)` and that
  the `/?*` behaviour has no `RealtimeLogConfigArn`.
- **Add**: the log bucket exists with `BLOCK_ALL`, `enforceSSL`, `BUCKET_OWNER_ENFORCED`, a
  lifecycle expiration rule, and `DeletionPolicy: Delete`.
- **Add**: the log bucket policy allows `delivery.logs.amazonaws.com` `s3:PutObject` with the
  `aws:SourceAccount` condition, and never an Allow to a wildcard principal.
- **Add** (in the new `LogDeliveryStack` test, us-east-1): one `CfnDeliverySource`
  (`ACCESS_LOGS`), one `CfnDeliveryDestination` (`OutputFormat: json`), one `CfnDelivery` with
  the expected `RecordFields`.
- **Keep unchanged**: the six-functions/arm64/JSON-logging/log-group tests still pass
  (`process_analytics` is still one of the six), the alarm test (threshold still 10), the
  DynamoDB hardening tests, and everything CloudFront/Cognito/API.
- The six-functions count is unchanged: `process_analytics` is rewritten, not removed.

### 8.2 Rust (`lambda/process_analytics`)

Unit tests over the pure functions, no AWS calls (FR-12.3):

- A valid short-link 302 GET -> `Some(link_id)`.
- An `/api/links` request -> `None`.
- An `/assets/main.js` request -> `None`.
- `/auth/callback` -> `None`; `/index.html` -> `None`; `/` -> `None`.
- A 404 on a short-link path -> `None`.
- A 200 on a short-link path -> `None`.
- A non-GET (HEAD) 302 -> `None`.
- A malformed/truncated line and a `#Fields:` header line -> skipped, no panic.
- `aggregate`: a file with 50 hits on one link and 3 on another -> `{a:50, b:3}` (proves
  FR-7's one-write-per-link aggregation input).

Idempotency test (FR-12.4): a small test of the claim logic proving the second claim of the
same object key is rejected and results in zero additional increments. This can be a pure test
of the "was already claimed -> skip" branch, or an integration-style test with a DynamoDB
local/mock; the pure branch test is sufficient for CI and is the one required.

## 9. Post-deploy verification plan (FR-12, satisfies acceptance criterion)

1. `export PATH="$HOME/.cargo/bin:$PATH"` (else cargo-lambda-cdk falls back to absent Docker
   and every CDK test fails with `FailedToBundleAsset ... docker ENOENT`).
2. `npm test` green; `cargo test` green (parser + idempotency).
3. `cdk synth`: confirm zero `AWS::Kinesis::Stream` and zero
   `AWS::CloudFront::RealtimeLogConfig` in the template.
4. Deploy (`cdk deploy --all --profile default`). Note the delivery constructs deploy in the
   new us-east-1 stack.
5. In the console, confirm the distribution's Logging tab shows the v2 S3 delivery as Enabled.
6. Verify the sparse-GSI claim before trusting the marker design: write a `PROCESSED#` marker
   row by hand (or let the first log object create one), then confirm `list_urls` for the
   admin user does NOT return it. This proves markers stay out of `TimeStampIndex`.
7. Shorten a fresh link, visit it 3 times.
8. Wait for delivery. CloudFront usually delivers a standard log file within a few minutes,
   and AWS documents up to ~an hour worst case. Record the observed latency in minutes in the
   PR description.
9. Confirm the link's `Clicks` incremented by exactly 3 (one `UpdateItem`, not three).
10. Re-invoke the function on the same object (or wait for a natural redelivery if one occurs)
    and confirm `Clicks` did NOT change again -- the idempotency guard held.
11. Confirm asset/API traffic during the window did NOT increment any counter.

## 10. README updates (FR-12.5)

- Data Flow section 2: replace "The realtime log is sent from CloudFront to a Kinesis stream.
  The process_analytics function increments the visit count." with the S3 pipeline: CloudFront
  writes a standard access-log file to the log bucket; an S3 ObjectCreated event invokes
  process_analytics, which filters to short-link 302s and increments Clicks. Note the
  seconds-to-minutes latency as an expected property.
- Infrastructure list: remove the Kinesis entry; change the `processAnalyticsLambda`
  description from "Handles the CF access logs from kinesis" to "consumes CloudFront S3 access
  logs on ObjectCreated and increments click counts"; add the log bucket under S3.
- ASCII diagram: replace the `[Kinesis]` node with `[S3 access logs]` feeding `[Lambda]` on
  ObjectCreated. Use `>` not `->` and no em dashes in the prose per house style.

## Requirements coverage

- FR-1 eliminate Kinesis: §1, §8.1.
- FR-2 preserve counting: §3, §4.4.
- FR-3 redirect untouched: §1 (behaviour kept), §3.3.
- FR-4 count only short-link 302s: §4.2.
- FR-5 idempotency, lose-not-inflate: §7.
- FR-6 parse by name, pure, no panic: §4.3.
- FR-7 one write per distinct link: §3.2 step 6-7, §4.3 aggregate, §4.4.
- FR-8 disposable log bucket + lifecycle: §5.
- FR-9 preserve alarm: §6.
- FR-10 conventions + opaque errors: §3.1, §4.4.
- FR-11 out of scope, note where aggregates would live: §4.1.
- FR-12 tests + docs: §8, §9, §10.
