# Design: Modernize and Harden the krtk.rs Codebase

## Overview

This document describes the technical approach for each requirement. Work is split into two deployment phases:

- **Phase A (Code & Dependencies):** FR-1 through FR-10 — Rust workspace modernization, dependency upgrades, CDK modernization, and test replacement. No infrastructure changes; deployed artefacts remain functionally identical.
- **Phase B (Infrastructure Hardening):** FR-11 through FR-14 — S3 OAC, DynamoDB hardening, ARM64, and log groups. Deployed as a separate `cdk deploy` to enable independent rollback.

---

## FR-1: Verify Build Toolchain

### Approach

`cargo-lambda` is already installed. Verify it works by running a workspace build targeting arm64.

### Steps

1. Verify: `cargo lambda --version` confirms installation.
2. Verify: `cargo lambda build --release --arm64 --workspace` succeeds for all members.
3. Verify: `cdk synth` produces valid CloudFormation (RustFunction invokes cargo-lambda internally).

### Key Decisions

- No installation needed — cargo-lambda is already available.
- The `cargo-lambda-cdk` construct handles invoking it; no CDK changes needed for this step alone.

---

## FR-2: Consolidate Rust Workspace Dependencies

### Approach

Add a `[workspace.dependencies]` section to the root `Cargo.toml`. Each member crate switches to `dep.workspace = true` for shared dependencies. The `lambda_http` version conflict is resolved by picking the latest compatible version (currently 0.14.x) across all crates.

### Root Cargo.toml Changes

```toml
[workspace]
resolver = "2"
members = [
  "shared",
  "lambda/create_link",
  "lambda/get_links",
  "lambda/visit_link",
  "lambda/process_analytics",
]

[workspace.dependencies]
lambda_http = "0.14"
lambda_runtime = "0.13"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
aws-config = "1"
aws-sdk-dynamodb = "1"
aws-sdk-secretsmanager = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
aws_lambda_events = { version = "0.15", default-features = false, features = ["kinesis"] }
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "http2", "json"] }
chrono = "0.4"
thiserror = "2"
serde_dynamo = { version = "4", features = ["aws-sdk-dynamodb+1"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }
```

### Migration Notes

- `lambda_http` 0.14 changes: the `Request` type moved some re-exports. The handlers in `create_link`, `get_links`, and `visit_link` already use `lambda_http::Request` — verify that `RequestExt`, `RequestPayloadExt`, and `IntoResponse` are still at the same paths.
- `process_analytics` uses `lambda_runtime` (not `lambda_http`) since it handles Kinesis events directly — this remains separate and correct.
- The duplicate `lambda/create_link` member entry is simply removed.

---

## FR-3: Pin Rust Toolchain and Upgrade to Edition 2024

### Approach

Create `rust-toolchain.toml` at the workspace root.

#### What is `rust-toolchain.toml`?

It's a Rust project convention (recognized by `rustup`) that declares which compiler version and targets the project requires. When anyone — you, CI, a future contributor — runs `cargo build` in this directory, `rustup` reads this file and automatically installs/selects the correct toolchain. Without it, builds silently use whatever Rust version happens to be installed on the machine, which can lead to:
- "works on my machine" failures when CI or another dev has a different version
- Accidentally using nightly features or hitting edition incompatibilities
- No guarantee the cross-compilation target (aarch64 for Lambda) is installed

It's the Rust equivalent of `.nvmrc` for Node or `.python-version` for pyenv — a single file that makes the build environment reproducible.

```toml
[toolchain]
channel = "stable"
targets = ["aarch64-unknown-linux-gnu"]
```

Then update every `Cargo.toml` from `edition = "2021"` to `edition = "2024"`.

### Edition 2024 Migration Considerations

Key semantic changes in edition 2024:
- **`impl Trait` lifetime capture rules** — `impl Trait` in return position now captures all in-scope lifetimes by default. The handlers return `Result<impl IntoResponse, Error>` which already has no lifetime parameters, so no change needed.
- **`unsafe` attribute syntax** — not applicable (no unsafe code in this workspace).
- **`gen` keyword reservation** — scan for identifiers named `gen`; none found.
- **Tail expression temporary scoping** — may affect temporaries in match arms. Run `cargo fix --edition` to auto-apply required changes.

### Steps

1. Create `rust-toolchain.toml`.
2. Run `cargo fix --edition` across the workspace.
3. Update all `edition` fields to `"2024"`.
4. Verify `cargo build --workspace` and `cargo clippy --workspace`.

---

## FR-4: Migrate to Rinja (Askama Successor)

### Approach

`askama` 0.12.1 is frozen on crates.io — the maintainers forked it as **rinja** where all active development continues. The API is nearly identical (same derive macro, same Jinja2 template syntax, same filters system). We migrate to `rinja`.

### Key Changes (askama 0.12 → rinja latest)

1. Rename dependency: `askama` → `rinja` in `Cargo.toml`.
2. Update imports: `use askama::Template` → `use rinja::Template`.
3. Update error type: `askama::Error` → `rinja::Error`.
4. The `filters` module API is stable; custom filters remain compatible.
5. Template files (`.html`) require no changes — Jinja2 syntax is identical.
6. The `pub use askama::{Template, Error}` in `templates.rs` becomes `pub use rinja::{Template, Error}`.

### Verification

Render each template with known inputs in a unit test and compare output to a captured baseline from the current version.

---

## FR-5: Pin AWS SDK Behaviour Version

### Approach

Remove the `behavior-version-latest` feature from `aws-config` in `[workspace.dependencies]` and pass an explicit version in each Lambda's `main()`:

```rust
let config = aws_config::defaults(aws_config::BehaviorVersion::v2026_01_12())
    .load()
    .await;
```

`v2026_01_12` is the current latest stable BehaviorVersion (as of aws-config 1.10.x). It enables retries by default for AWS SDK clients and sets a 3.1s connect timeout. Available versions in descending order:
- `v2026_01_12` — retries + connect timeout (current latest, what `latest()` resolves to)
- `v2025_08_07` — adds HTTP(S) proxy env var support (deprecated)
- `v2025_01_17` — updated HTTP/TLS stack (deprecated)
- `v2024_03_28` — stalled stream protection for uploads (deprecated)
- `v2023_11_09` — initial version (deprecated)

We pin to `v2026_01_12` rather than calling `latest()` because pinning guarantees that a future SDK upgrade won't silently change retry/timeout/credential behaviour. When a newer BehaviorVersion is eventually released, the deprecation warning at compile time signals us to evaluate and consciously adopt it.

This replaces the current:
```rust
let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
```

### Impact

All four Lambda handlers (`create_link`, `get_links`, `visit_link`, `process_analytics`) have this identical pattern in `main()`. The change is mechanical.

---

## FR-5b: Update All Rust Dependencies

### Approach

1. After workspace consolidation (FR-2), run `cargo update` to pull latest semver-compatible versions.
2. Run `cargo outdated` (if available) to identify crates with new major versions available.
3. For each major-version-bump candidate:
   - If the migration is documented and mechanical: bump the pin and adapt.
   - If it requires significant API rewrite: document as a follow-up and keep the current pin.
4. Run `cargo clippy --workspace -- -D warnings` and fix any new lints.
5. Run `cargo audit` to check for advisories.

### Known Crates Likely to Have Updates

| Crate | Current | Notes |
|---|---|---|
| `reqwest` | 0.12 | Stable, minor bumps only expected |
| `scraper` | 0.22 | Check for HTML parser changes |
| `chrono` | 0.4 | Stable, unlikely breaking |
| `cuid2` | 0.1 | Check if 1.0 released |
| `aws_lambda_events` | 0.15 | May have struct changes |
| `lambda_runtime` | 0.13 | Check alignment with lambda_http |

---

## FR-6: Adopt serde_dynamo for DynamoDB Serialization

### Approach

Replace the 90-line `TryFrom<HashMap<String, AttributeValue>>` impl with serde_dynamo's `from_item()` / `from_items()`.

#### Why serde_dynamo?

`serde_dynamo` (by Zenlist) is the de-facto standard for Rust ↔ DynamoDB serialization. It's the most widely used crate for this purpose in the Rust/AWS ecosystem with 139 GitHub stars and active maintenance (206 commits, last updated 2025). It bridges serde's `Serialize`/`Deserialize` traits directly to DynamoDB's `AttributeValue` types, exactly the same way `serde_json` bridges to JSON. The AWS Rust SDK team does not provide their own serde integration — `serde_dynamo` fills that gap and is referenced in community guides and the Rust Lambda book.

### Design

Add `#[derive(Deserialize)]` to `ShortUrl` (it already has `Serialize`) with serde rename attributes to match DynamoDB attribute names:

```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct ShortUrl {
    #[serde(rename = "LinkId")]
    pub link_id: String,
    #[serde(rename = "OriginalLink")]
    original_link: String,
    #[serde(rename = "Clicks")]
    clicks: u32,
    #[serde(rename = "Title")]
    title: Option<String>,
    #[serde(rename = "Description")]
    description: Option<String>,
    #[serde(rename = "ContentType")]
    content_type: Option<String>,
    #[serde(rename = "Image")]
    image: Option<String>,
    #[serde(rename = "TimeStamp")]
    timestamp: i64,
}
```

### list_urls Changes

```rust
// Before (silently discards broken items):
if let Ok(short_url) = ShortUrl::try_from(item) {
    short_urls.push(short_url);
}

// After (propagates error):
let short_urls: Vec<ShortUrl> = serde_dynamo::from_items(items)?;
```

The `SortKey` attribute is present in DynamoDB items but not needed in the `ShortUrl` struct — serde_dynamo's default is to ignore unknown fields (`#[serde(deny_unknown_fields)]` is NOT set), so this works transparently.

### shorten_url Changes

The `put_item` call currently builds the item manually with `AttributeValue::S(...)`. This can optionally be replaced with `serde_dynamo::to_item(&short_url)?`, but since the put_item also conditionally includes optional fields and has the `condition_expression`, keeping the manual construction is acceptable for now. The priority is the read path (where the silent discard bug lives).

---

## FR-7: Typed Error Handling with thiserror

### Approach

Create `shared/src/error.rs` with a typed error enum:

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("Validation error: {0}")]
    Validation(String),

    #[error("URL not found: {0}")]
    NotFound(String),

    #[error("Data store operation failed")]
    Database(#[source] aws_sdk_dynamodb::Error),

    #[error("Data serialization failed")]
    Serialization(#[source] serde_dynamo::Error),

    #[error("URL safety check failed: {0}")]
    SafeBrowsing(String),

    #[error("Failed to render response")]
    Template(#[source] rinja::Error),

    #[error("Internal error")]
    Internal(String),
}
```

**Note on error messages:** The `#[error("...")]` strings are what gets returned to the client via `Display`. They deliberately avoid naming internal services (DynamoDB, serde_dynamo, rinja) — the user doesn't need to know our storage backend. The original error is still captured via `#[source]` for structured logging (`{:?}` in tracing), so operators see the full chain in CloudWatch while clients see only a generic message.

### HTTP Status Mapping

In each Lambda handler, map `AppError` variants to HTTP status codes:

```rust
impl AppError {
    pub fn status_code(&self) -> StatusCode {
        match self {
            Self::Validation(_) | Self::SafeBrowsing(_) => StatusCode::BAD_REQUEST,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}
```

### Migration Path

1. Create `shared/src/error.rs` and add `pub mod error;` to `lib.rs`.
2. Change `UrlShortener` methods from `Result<T, String>` to `Result<T, AppError>`.
3. Change `ShortenUrlRequest::validate` from `Result<Self, String>` to `Result<Self, AppError>`.
4. Update Lambda handlers to use `AppError` — the match arms that currently check `Err(e)` become typed and can produce correct status codes.
5. HTMX error rendering path: `ErrorPopup { message: e.to_string() }` — the Display impl from thiserror provides the message.

---

## FR-8: Modernize Node.js / CDK Dependencies

### Approach

```bash
# Remove stray cdk dependency
npm uninstall cdk

# Update CDK packages
npm install aws-cdk-lib@latest cargo-lambda-cdk@latest
npm install -D aws-cdk@2.1136.0 typescript@latest @types/node@latest @types/jest@latest

# Rebuild
npm run build
```

### Verification

- `cdk synth` produces valid CloudFormation.
- `npm run build` (tsc) succeeds with updated type definitions.
- `cdk diff` shows no unexpected changes from the dependency update alone.

---

## FR-9: Review and Adopt CDK Feature Flags

### Approach

1. Run `cdk context --clear` followed by `cdk doctor` to see recommended flags.
2. Compare the current `cdk.json` context against the CDK source's `FEATURE_FLAGS.md` for the installed version.
3. Add any new recommended flags.
4. For each new flag: run `cdk diff` to verify no resource replacement.
5. If a flag causes replacement: exclude it with a `// EXCLUDED:` comment explaining why.

### Known Considerations

The existing `cdk.json` already has 50+ flags from an older CDK version. The main additions for 2.180+ are likely around:
- S3 bucket naming conventions
- Lambda logging defaults
- ECS/EKS defaults (not relevant here)

Since this project uses S3, Lambda, CloudFront, DynamoDB, and API Gateway, focus the audit on those service flags.

---

## FR-10: Replace Placeholder CDK Tests

### Approach

Rewrite `test/krtk-rs.test.ts` using `Template.fromStack` with `hasResourceProperties` assertions.

### Test Structure

```typescript
import * as cdk from 'aws-cdk-lib';
import { Template } from 'aws-cdk-lib/assertions';
import { KrtkRsStack } from '../lib/krtk-rs-stack';
import { SecretsStack } from '../lib/secrets-stack';

describe('KrtkRsStack', () => {
  let template: Template;

  beforeAll(() => {
    const app = new cdk.App();
    // SecretsStack must be created first as it provides the Google API key secret
    const secretsStack = new SecretsStack(app, 'TestSecretsStack', {
      env: { region: 'us-west-2', account: '503716878456' }
    });
    const stack = new KrtkRsStack(app, 'TestStack', {
      certificateArn: 'arn:aws:acm:us-east-1:503716878456:certificate/test-cert-id',
      googleApiKeySecret: secretsStack.googleApiKeySecret,
      env: { region: 'us-west-2', account: '503716878456' },
      crossRegionReferences: true,
    });
    template = Template.fromStack(stack);
  });

  test('DynamoDB table with correct key schema', () => { ... });
  test('Lambda functions created with correct runtime', () => { ... });
  test('CloudFront distribution with custom domain', () => { ... });
  test('HTTP API with correct routes', () => { ... });
  test('S3 bucket for hosting', () => { ... });
  test('Kinesis stream for analytics', () => { ... });
});
```

### Notes

- The stack requires `certificateArn` and `googleApiKeySecret` as props — tests will use dummy/mock values.
- `SecretsStack` is read to understand its export shape.

---

## FR-11: Private S3 Bucket with CloudFront OAC

### Approach

This is the most complex infrastructure change because the S3 website endpoint and the S3 REST endpoint behave differently.

### Current Architecture

```
CloudFront → S3StaticWebsiteOrigin (HTTP) → public bucket with websiteIndexDocument
```

The S3 website endpoint automatically:
- Serves `index.html` for `/` requests
- Returns HTML-formatted 404 errors
- Resolves extensionless objects as-is (e.g. `/terms` serves the `terms` object)

### Target Architecture

```
CloudFront → S3BucketOrigin.withOriginAccessControl() → private bucket
```

The S3 REST endpoint (used with OAC):
- Does NOT resolve index documents automatically (CloudFront's `defaultRootObject` only applies to the root `/`, not subdirectories)
- Returns XML-formatted errors
- Extensionless objects work identically (just `GetObject` by key)

### Implementation

```typescript
import { S3BucketOrigin } from 'aws-cdk-lib/aws-cloudfront-origins';

const hostingBucket = new Bucket(this, 'hostingBucket', {
  removalPolicy: cdk.RemovalPolicy.DESTROY,
  autoDeleteObjects: true,
  bucketName: 'krtk.rs',
  // publicReadAccess removed
  // blockPublicAccess defaults to BLOCK_ALL
  // websiteIndexDocument removed (not needed with OAC)
});

const s3Origin = S3BucketOrigin.withOriginAccessControl(hostingBucket);
```

### Handling the Differences

| Behaviour | Website Endpoint | REST + OAC | Solution |
|---|---|---|---|
| Root `/` → `index.html` | Automatic | Only via `defaultRootObject` | Set `defaultRootObject: 'index.html'` on distribution |
| 404 fallback to `/` (SPA) | N/A (custom error in CF) | Same CF `errorResponses` | Keep existing `errorResponses` config — it operates at CF level |
| `/terms` (extensionless) | Serves object `terms` | Serves object `terms` | No change needed — both serve by key name |
| `/privacy` (extensionless) | Serves object `privacy` | Serves object `privacy` | No change needed |
| Content-Type for extensionless | Set by S3 metadata | Same | Already handled by CF `ResponseHeadersPolicy` overriding content-type |
| `/assets/*` | Normal S3 GET | Normal S3 GET | No change — works with OAC |

### Critical Verification

- `defaultRootObject` ONLY applies to the bare distribution root (`/`). Since this is an SPA with the 404→`/` fallback, subpaths like `/foo` get a 404 from S3, CF maps it to `/` (which serves `index.html`), and the SPA router handles it. This matches current behaviour.
- The `/?*` behaviour (link redirect via API Gateway) is a separate CF behaviour and unaffected.

---

## FR-12: DynamoDB Table Hardening

### Approach

Simple property changes on the existing `TableV2` construct:

```typescript
const linkDatabase = new TableV2(this, 'linkTable', {
  partitionKey: {
    name: 'LinkId',
    type: AttributeType.STRING,
  },
  removalPolicy: cdk.RemovalPolicy.RETAIN,
  pointInTimeRecovery: true,
  deletionProtection: true,
});
```

### Deployment Safety

- `removalPolicy` is a CDK-level concept (CloudFormation `DeletionPolicy`) — changing it from DELETE to RETAIN is a metadata-only update, no data movement.
- `pointInTimeRecovery` and `deletionProtection` are in-place attribute updates on the existing table.
- `cdk diff` will confirm no replacement.

---

## FR-13: ARM64 / Graviton Lambda Architecture

### Approach

Add `architecture` to each `RustFunction` construct:

```typescript
import { Architecture } from 'aws-cdk-lib/aws-lambda';

const createLinkLambda = new RustFunction(this, 'createLink', {
  manifestPath: 'lambda/create_link/Cargo.toml',
  runtime: 'provided.al2023',
  architecture: Architecture.ARM_64,
  // ... rest unchanged
});
```

### Build Chain

`cargo-lambda-cdk`'s `RustFunction` detects the `architecture` prop and passes `--arm64` to `cargo lambda build` automatically. No manual cross-compilation flags needed in `Cargo.toml` or build scripts.

### rust-toolchain.toml Alignment

The `aarch64-unknown-linux-gnu` target in `rust-toolchain.toml` (FR-3) ensures the toolchain has the correct target installed. cargo-lambda uses Zig for actual cross-linking, so the Rust target is for type checking/IDE support.

### Deployment Consideration

Changing Lambda architecture is an **in-place update** — CloudFormation replaces the function configuration but preserves the function name and ARN. There may be a brief cold start on the first invocation post-deploy.

---

## FR-14: Explicit Log Groups with Retention

### Approach

Create explicit `LogGroup` constructs for the three Lambdas that currently lack them (`createLink`, `getLinks`, `visitLink`). The `processAnalytics` Lambda already has one.

```typescript
const createLinkLogGroup = new LogGroup(this, 'createLinkLogGroup', {
  logGroupName: `/aws/lambda/${createLinkLambda.functionName}`,
  retention: cdk.aws_logs.RetentionDays.ONE_WEEK,
  removalPolicy: cdk.RemovalPolicy.DESTROY,
});
createLinkLogGroup.node.addDependency(createLinkLambda);
```

Repeat for `getLinks` and `visitLink`.

### JSON Structured Logging

All four handlers already use `tracing::init_default_subscriber()` from `lambda_http`/`lambda_runtime`. This produces JSON-formatted logs when running in Lambda (the `tracing` crate's Lambda layer auto-detects the environment). Verify by checking CloudWatch output post-deploy.

If the default subscriber doesn't produce JSON, add explicit configuration:

```rust
tracing_subscriber::fmt()
    .json()
    .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
    .init();
```

### Handling Existing Implicit Log Groups

This is a brand-new production deployment (single-user project). Delete any existing implicit log groups before deploy to avoid CloudFormation name conflicts. No historical log data needs to be preserved.

```bash
# Pre-deploy cleanup (run manually):
aws logs delete-log-group --log-group-name /aws/lambda/<createLink-fn-name> --region us-west-2
aws logs delete-log-group --log-group-name /aws/lambda/<getLinks-fn-name> --region us-west-2
aws logs delete-log-group --log-group-name /aws/lambda/<visitLink-fn-name> --region us-west-2
```

This is safe because:
- The project is single-user (Darko only)
- No production data in the logs needs preserving
- The explicit groups will be recreated immediately by the deploy

---

## Deployment Sequencing

### Phase A Deployment (FR-1 through FR-10)

```
1. Install cargo-lambda (FR-1)
2. Apply all Rust workspace changes (FR-2, FR-3, FR-4, FR-5, FR-5b, FR-6, FR-7)
3. cargo build --workspace && cargo clippy --workspace
4. cargo tree --workspace -i lambda_http (verify single version)
5. Apply CDK/Node changes (FR-8, FR-9, FR-10)
6. npm install && npm run build && npm test
7. cdk diff (expect: only Lambda code asset hash changes)
8. cdk deploy --all
9. Manual verification: shorten, list, redirect, analytics
```

### Phase B Deployment (FR-11 through FR-14)

```
1. Apply infrastructure CDK changes (FR-11, FR-12, FR-13, FR-14)
2. npm run build && npm test
3. cdk diff (expect: bucket policy change, OAC addition, architecture change, log groups)
4. Verify no resource REPLACEMENTS in diff output
5. cdk deploy --all
6. Manual verification: shorten, list, redirect, analytics
7. Verify /terms and /privacy still serve correctly
8. Verify CloudWatch logs appear in the new explicit log groups
```

---

## Risk Mitigation

| Risk | Mitigation |
|---|---|
| OAC switch breaks extensionless paths | `/terms` and `/privacy` already have dedicated CF behaviours with ResponseHeadersPolicy; they request objects by exact key name regardless of origin type |
| Edition 2024 breaks compilation | `cargo fix --edition` handles most changes; remainder is manual and scoped to lifetime capture rules |
| lambda_http 0.14 API changes break handlers | Changes are minor re-export moves; `RequestExt` and `IntoResponse` APIs are stable |
| DynamoDB RETAIN prevents future cleanup | Intentional — this is production data. Table can be manually deleted via console if needed |
| Existing implicit log groups conflict | Delete them pre-deploy or import; documented in deployment steps |
| ARM64 cold start regression | Monitor via CloudWatch metrics post-deploy; Graviton generally has faster cold starts for Rust |

---

## Files Modified Summary

### Rust (Phase A)

| File | Changes |
|---|---|
| `Cargo.toml` (root) | Fix members, add `[workspace.dependencies]` |
| `rust-toolchain.toml` | New file |
| `shared/Cargo.toml` | edition 2024, workspace deps, rinja, serde_dynamo, thiserror |
| `shared/src/lib.rs` | Add `pub mod error;` |
| `shared/src/error.rs` | New file — `AppError` enum |
| `shared/src/core.rs` | Remove `TryFrom` impl, use serde_dynamo, return `AppError` |
| `shared/src/templates.rs` | `askama` → `rinja` import |
| `shared/src/response.rs` | Accept `AppError` for error responses |
| `shared/src/safe_browsing.rs` | Return `AppError` instead of `String` |
| `lambda/create_link/Cargo.toml` | edition 2024, workspace deps |
| `lambda/create_link/src/main.rs` | BehaviorVersion pin, typed errors |
| `lambda/get_links/Cargo.toml` | edition 2024, workspace deps |
| `lambda/get_links/src/main.rs` | BehaviorVersion pin, typed errors |
| `lambda/visit_link/Cargo.toml` | edition 2024, workspace deps |
| `lambda/visit_link/src/main.rs` | BehaviorVersion pin, typed errors |
| `lambda/process_analytics/Cargo.toml` | edition 2024, workspace deps |
| `lambda/process_analytics/src/main.rs` | BehaviorVersion pin, typed errors |

### CDK / Node (Phase A)

| File | Changes |
|---|---|
| `package.json` | Remove `cdk`, update versions |
| `cdk.json` | New feature flags |
| `test/krtk-rs.test.ts` | Full rewrite with real assertions |

### CDK (Phase B)

| File | Changes |
|---|---|
| `lib/krtk-rs-stack.ts` | OAC, RETAIN, ARM64, log groups |

---

## Dead Code Removal

- `lambda/visit_link/src/http_handler.rs` — This is the stock cargo-lambda-generated hello-world handler. It is unused (the real handler is in `main.rs`). **Delete it.**
