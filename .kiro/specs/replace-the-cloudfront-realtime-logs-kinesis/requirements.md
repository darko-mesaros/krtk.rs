# Requirements: Replace CloudFront realtime-logs > Kinesis analytics with S3 access logs

## Introduction

krtk.rs counts clicks on short links with a decoupled analytics pipeline that today runs
CloudFront realtime logs > Kinesis Data Stream > `process_analytics` Lambda > DynamoDB
`UpdateItem`. The pipeline works, but the Kinesis stream is billed per stream-hour whether
or not data flows through it. Cost Explorer for this account confirms the charge:

- Aug 2026: `USW2-OnDemand-StreamHour` $16.59 (414.8 stream-hours). Cheaper only because
  the stream did not exist for the whole month.
- Sep 1-18 2026: `USW2-OnDemand-StreamHour` $17.28 (432 stream-hours, exactly 24h/day).
- Data volume across both periods is under 1 MB/month (`BilledIncomingBytes` fractions of
  a cent). At $0.040/stream-hour that projects to $29.76 for a 31-day month.

99.998% of the bill is the per-stream-hour charge for the stream existing, not the data
flowing, so sampling-rate changes cannot help. CloudFront charges nothing for standard
access logging; the replacement pipeline pays only S3 storage, S3 PUTs, and Lambda
invocations, which is cents per month at roughly 1,000 redirects/day.

This change replaces the Kinesis pipeline with CloudFront standard access logging to a
dedicated S3 bucket, consumed by a Lambda triggered on `s3:ObjectCreated`. It is a
like-for-like replacement of click counting: the frontend still reads the same `Clicks`
attribute and the redirect path is untouched. Richer per-link analytics (country, referer,
user-agent) are explicitly out of scope and deferred; this change exists so the cost saving
can be verified in isolation.

The accepted trade-off is latency: click counts move from seconds (streamed) to minutes
(CloudFront delivers standard log files roughly every few minutes, up to about an hour).
This is documented as a property, not a regression to fix.

## Glossary

- **Redirect path**: the `visit_link` Lambda behind the CloudFront `/?*` behaviour. It does
  one DynamoDB `GetItem` and returns a 302. It never writes.
- **Analytics path**: the decoupled pipeline that increments `Clicks`. A failure here must
  never affect the redirect path.
- **Short-link hit**: a CloudFront access-log record for a 302 redirect served from a
  short-link path (a single path segment such as `/k120oizr`), excluding site chrome and
  API traffic.
- **Log object**: one CloudFront standard access-log file delivered to the log bucket, which
  fires exactly one `s3:ObjectCreated` event.

## Requirements

### FR-1: Eliminate the Kinesis stream-hour charge

**User story:** As the operator, I want the per-stream-hour Kinesis charge gone, so that the
analytics pipeline costs cents per month instead of ~$30.

#### Acceptance criteria

1. WHEN the stack is synthesized THEN the template SHALL contain zero
   `AWS::Kinesis::Stream` resources.
2. WHEN the stack is synthesized THEN the template SHALL contain zero
   `AWS::CloudFront::RealtimeLogConfig` resources.
3. WHEN the stack is synthesized THEN the `/?*` CloudFront behaviour SHALL NOT carry a
   `RealtimeLogConfigArn`.
4. WHEN the stack is synthesized THEN no `AWS::Lambda::EventSourceMapping` SHALL reference a
   Kinesis stream.
5. WHEN `process_analytics` is built THEN `Cargo.toml` SHALL enable the `aws_lambda_events`
   `s3` feature and SHALL NOT enable the `kinesis` feature.
6. WHEN the CDK source is reviewed THEN the now-unused Kinesis and realtime-log imports
   (`Endpoint`, `RealtimeLogConfig`, `Stream`, `StreamMode`, `KinesisEventSource`,
   `StartingPosition`) SHALL be removed.

### FR-2: Preserve click counting end to end

**User story:** As a link owner, I want a visit to my short link to still increment its click
count, so that the migration is invisible in the UI.

#### Acceptance criteria

1. WHEN a short link is visited and its access-log record is delivered THEN the
   analytics Lambda SHALL increment that link's `Clicks` attribute by exactly the number of
   hits in the delivered log object.
2. WHEN a link is created THEN `Clicks` SHALL start at 0, unchanged from today.
3. WHEN `get_links` returns a link THEN the `Clicks` value SHALL appear in the JSON and the
   rendered Clicks column exactly as before; the `/api/links` wire format SHALL NOT change.
4. IF the target `LinkId` no longer exists in the table THEN the increment for that link
   SHALL be skipped without failing the invocation (the existing
   `attribute_exists(LinkId)` conditional behaviour is preserved).

### FR-3: Redirect path is untouched

**User story:** As a visitor, I want redirects to keep working with the same latency and
reliability, so that analytics changes never break resolution.

#### Acceptance criteria

1. WHEN this change ships THEN `visit_link` SHALL still perform a single `GetItem` and
   return a 302, with no click-counting logic added to it.
2. IF the analytics pipeline is failing for any reason THEN redirects SHALL continue to
   succeed, because counting stays fully decoupled from the request path.
3. WHEN the CloudFront distribution is synthesized THEN the `/?*` redirect behaviour SHALL
   retain its existing origin, cache policy, and origin-request policy; only the
   realtime-log attachment is removed.

### FR-4: Count only short-link redirects

**User story:** As the operator, I want counters to reflect real redirects only, so that
asset and API traffic never inflate them.

#### Acceptance criteria

1. WHEN a log object is processed THEN the parser SHALL count a record only IF its status is
   302 AND its request path is a single short-link segment.
2. WHEN a log record's path matches `/api/*`, `/assets/*`, `/auth/*`, `/index.html`,
   `/terms`, `/privacy`, or the root `/` THEN that record SHALL be excluded from counting.
3. WHEN a log record has any non-302 status (200, 404, 3xx other than 302, 5xx) THEN it
   SHALL be excluded from counting.
4. WHEN standard logging is configured THEN the operator SHALL understand it covers the
   ENTIRE distribution, not just the `/?*` behaviour the old realtime config was scoped to;
   filtering to short-link 302s is therefore mandatory, not optional. This is the single
   biggest behavioural difference from the current design.

### FR-5: Idempotent counting under at-least-once delivery

**User story:** As the operator, I want a redelivered log object to not double-count, so that
counters stay trustworthy.

#### Acceptance criteria

1. WHEN the same log object is delivered and processed more than once THEN the affected
   links SHALL be incremented for it exactly once.
2. IF the guard must choose between losing counts and inflating them THEN it SHALL lose
   rather than inflate; under-counting is acceptable, double-counting is not.
3. WHEN the guard's strategy is chosen THEN the design SHALL state explicitly whether a
   mid-file crash loses or inflates counts, and SHALL justify the choice.

### FR-6: Field-name parsing, no positional fragility

**User story:** As a maintainer, I want the log parser to address fields by name, so that a
CloudFront field-order change cannot silently corrupt counts or panic.

#### Acceptance criteria

1. WHEN a log record is parsed THEN fields SHALL be addressed by name, never by fixed column
   index. (The current Kinesis parser indexes `fields[2]`/`fields[3]` positionally and only
   works by coincidence; `fields[3]` panics on any format drift. That approach is not
   carried forward.)
2. WHEN a malformed, truncated, or header line is parsed THEN the parser SHALL skip it and
   SHALL NOT panic.
3. WHEN the parser is implemented THEN its core SHALL be a pure function over a log line or a
   decompressed buffer, testable with no AWS calls.

### FR-7: Aggregate per link, one write per distinct link

**User story:** As the operator, I want a log file with many hits on one link to cost one
DynamoDB write, not one per hit, so that the new pipeline is cheaper and calmer than the old
one-record-per-invocation model.

#### Acceptance criteria

1. WHEN a log object contains N short-link hits across M distinct link IDs THEN the Lambda
   SHALL issue at most M `UpdateItem` calls (one per distinct link, incrementing by that
   link's in-file hit count), never N.
2. WHEN a link appears K times in one log object THEN its counter SHALL be incremented by K
   in a single `UpdateItem`.

### FR-8: Disposable log bucket with lifecycle expiry

**User story:** As the operator, I want the log bucket to expire its objects and be safe to
tear down, so that raw logs do not accumulate cost or become a data-retention liability.

#### Acceptance criteria

1. WHEN the log bucket is created THEN it SHALL be a NEW dedicated bucket, never the
   `hostingBucket` (which is public-facing content behind OAC).
2. WHEN the log bucket is created THEN it SHALL block all public access and enforce SSL,
   matching the project's existing posture.
3. WHEN the log bucket is created THEN it SHALL carry a lifecycle rule that expires log
   objects, with a retention chosen and justified in the design.
4. WHEN the stack is deleted THEN the log bucket SHALL be destroyable (removalPolicy DESTROY
   plus autoDeleteObjects), because logs are reproducible, disposable telemetry, matching the
   `hostingBucket` precedent.

### FR-9: Preserve the invalid-URL alarm signal

**User story:** As the operator, I want to keep being alarmed when the analytics function
logs an unusual number of failures, so that a broken pipeline is still visible.

#### Acceptance criteria

1. WHEN a click increment fails THEN the analytics Lambda SHALL emit a `warn`-level log line,
   as today.
2. WHEN `warn` lines are emitted THEN a metric filter on the new function's log group
   (`$.level = warn`, namespace `KrtkRs`, metric `InvalidUrlWarnings`) SHALL feed an alarm
   equivalent to the current `invalidUrlAlarm`.
3. IF the failures-arrive-in-file-sized-batches model makes the current threshold of 10 in 5
   minutes inappropriate THEN the design SHALL state the new threshold and justify the
   change; otherwise the threshold SHALL stay 10 in 5 minutes.

### FR-10: Match existing infrastructure conventions

**User story:** As a maintainer, I want the new resources to look like the rest of the stack,
so that the codebase stays consistent.

#### Acceptance criteria

1. WHEN the analytics Lambda is defined THEN it SHALL be a `RustFunction`, ARM_64,
   `provided.al2023`, with `LoggingFormat.JSON` and an explicit `LogGroup` from
   `logGroupDefaults` (ONE_WEEK retention, DESTROY).
2. WHEN permissions are granted THEN they SHALL use the L2 grant methods
   (`bucket.grantRead`, `linkDatabase.grantWriteData`) for least privilege, and SHALL add
   whatever grant the idempotency guard requires (for example write access to the marker
   store).
3. WHEN error types or user-facing messages are written THEN they SHALL NOT name backend
   services; the existing opaque `AppError::Database` ("Data store operation failed") variant
   is reused, never a `DynamoDbError`-style name.

### FR-11: Out of scope this change

**User story:** As the operator, I want richer analytics deferred, so that the cost saving is
verifiable in isolation.

#### Acceptance criteria

1. WHEN this change ships THEN country, referer, and user-agent breakdowns SHALL NOT be
   implemented.
2. WHEN the logging configuration selects fields THEN it MAY include additional fields IF
   doing so is free, and the design SHALL note where such per-dimension aggregates would live
   given the no-sort-key constraint, but no aggregation beyond per-link click counts SHALL be
   built.
3. WHEN this change ships THEN the `linkTable` SHALL NOT gain a sort key (it is RETAIN +
   deletionProtection; adding one is a migration). Any future per-dimension aggregate must use
   synthetic `LinkId` partitions in the same table or a new table.

### FR-12: Tests and documentation updated

**User story:** As a maintainer, I want the test suite and README to reflect the new pipeline,
so that CI is green and the docs are not lying.

#### Acceptance criteria

1. WHEN `cdk synth` runs THEN it SHALL succeed and the synthesized template SHALL contain
   zero `AWS::Kinesis::Stream` and zero `AWS::CloudFront::RealtimeLogConfig` resources.
2. WHEN `npm test` runs (with `PATH` including `$HOME/.cargo/bin`) THEN it SHALL pass with the
   revised CDK assertions: the Kinesis-stream test, the realtime-log-config test, and the
   Kinesis event-source-mapping test SHALL be replaced with assertions for the log bucket,
   the standard-logging configuration, and the S3 event source; unrelated assertions SHALL be
   preserved.
3. WHEN `cargo test` runs THEN it SHALL pass, including new unit tests for the log-line
   parser covering at minimum: a valid short-link 302, an `/api/` request, an `/assets/`
   request, a non-302 status such as 404, and a malformed or truncated line that does not
   panic.
4. WHEN the idempotency guard is tested THEN a test SHALL prove that processing the same log
   object twice increments each affected link exactly once.
5. WHEN the README is updated THEN the Data Flow section, the Infrastructure list, and the
   ASCII architecture diagram SHALL describe the S3-access-log pipeline instead of Kinesis,
   and SHALL document the seconds-to-minutes latency change as an expected property.

## Non-functional requirements

- **Cost:** the recurring analytics cost SHALL drop from the ~$30/month Kinesis stream-hour
  charge to cents/month (S3 storage + PUTs + Lambda invocations).
- **Latency:** click counts MAY lag visits by minutes (up to ~an hour), versus seconds today.
  This is acceptable and SHALL be documented, not treated as a regression.
- **Safety:** `linkTable` retains RETAIN + deletionProtection + PITR and gains no sort key.
- **Isolation:** an analytics failure SHALL NOT affect redirect availability.

## Post-deploy verification plan (to be detailed in design)

Shorten a link, visit it, wait out the delivery window, confirm `Clicks` increments by the
expected amount, and record the observed delivery latency in minutes. Full steps land in
design.md.
