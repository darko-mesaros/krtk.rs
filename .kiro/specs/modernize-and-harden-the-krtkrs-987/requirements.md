# Requirements: Modernize and Harden the krtk.rs Codebase

## Overview

Behaviour-preserving modernization and security hardening of the krtk.rs URL shortener as prerequisite groundwork before Cognito authentication is added. When complete, the site must look and behave identically — no API contract change, no DynamoDB schema change, no short URL format change.

**Depends on:** Nothing (this is the first spec).
**Depended on by:** `i-want-to-introduce-cognito` (auth spec).

---

## Functional Requirements

### FR-1: Verify Build Toolchain

Verify that `cargo-lambda` (already installed) can build the workspace for arm64 and that `cdk synth` succeeds.

**Acceptance Criteria:**
- AC-1.1: `cargo lambda build --release --arm64` succeeds for all workspace members.
- AC-1.2: `cdk synth` completes without errors related to missing build tools.

---

### FR-2: Consolidate Rust Workspace Dependencies

Eliminate the dual `lambda_http` version conflict (0.13.0 in lambdas, 0.14.0 in `shared`) and centralize all shared dependencies under `[workspace.dependencies]`.

**Acceptance Criteria:**
- AC-2.1: `cargo tree --workspace -i lambda_http` shows exactly one version.
- AC-2.2: The root `Cargo.toml` defines `[workspace.dependencies]` for: `lambda_http`, `tokio`, `aws-config`, `aws-sdk-dynamodb`, `serde_json`, `aws-sdk-secretsmanager`.
- AC-2.3: Each member uses `dep.workspace = true` for shared dependencies.
- AC-2.4: The duplicate `lambda/create_link` entry in `[workspace] members` is removed.
- AC-2.5: `cargo build --workspace` succeeds with zero warnings about dependency resolution.

---

### FR-3: Pin Rust Toolchain and Upgrade to Edition 2024

Add `rust-toolchain.toml` to guarantee reproducible builds across environments and upgrade all crates to edition 2024.

**Acceptance Criteria:**
- AC-3.1: A `rust-toolchain.toml` exists at the workspace root specifying the `stable` channel and the `aarch64-unknown-linux-gnu` target.
- AC-3.2: All `Cargo.toml` files declare `edition = "2024"`.
- AC-3.3: `cargo build --workspace` succeeds under edition 2024 semantics.

---

### FR-4: Migrate to Rinja (Askama Successor)

Migrate from askama 0.12.1 (frozen) to rinja (its actively maintained successor), adapting imports and derive macros to the new crate name.

**Acceptance Criteria:**
- AC-4.1: `shared/Cargo.toml` depends on `rinja` (latest stable), not `askama`.
- AC-4.2: All templates in `shared/templates/` compile without errors.
- AC-4.3: The HTML output for the visit_link redirect page and the links-table HTMX partial are byte-identical to the 0.12.1 output (verified by rendering in tests or manual inspection).

---

### FR-5: Pin AWS SDK Behaviour Version

Replace the `behavior-version-latest` feature flag on `aws-config` with an explicit `BehaviorVersion` pin so SDK upgrades cannot silently change retry, timeout, or credential-resolution behaviour.

**Acceptance Criteria:**
- AC-5.1: No crate in the workspace uses the `behavior-version-latest` feature of `aws-config`.
- AC-5.2: Each Lambda's SDK client construction calls `BehaviorVersion::v2026_01_12()` (the current latest stable version) explicitly.
- AC-5.3: `cargo build --workspace` succeeds.

---

### FR-5b: Update All Rust Dependencies to Latest Compatible Versions

Run a full `cargo update` across the workspace and resolve any breaking or behavioural changes. This ensures all direct and transitive dependencies are at their latest semver-compatible versions, reducing technical debt and closing known CVEs before the auth work adds new surface area.

**Acceptance Criteria:**
- AC-5b.1: `cargo update` is run and `Cargo.lock` reflects the latest compatible versions for all dependencies.
- AC-5b.2: Any crate with a major-version bump available (beyond the current pin range) is evaluated: if the upgrade is straightforward, bump the pin in `[workspace.dependencies]`; if it requires significant refactoring, document it as a follow-up.
- AC-5b.3: Breaking behavioural changes in updated crates (especially `reqwest`, `scraper`, `lambda_runtime`, `aws_lambda_events`, `chrono`) are identified and code is adapted.
- AC-5b.4: `cargo build --workspace` succeeds and `cargo clippy --workspace` produces no new warnings.
- AC-5b.5: `cargo audit` (if installed) reports no known vulnerabilities in the updated dependency tree.

---

### FR-6: Adopt serde_dynamo for DynamoDB Serialization

Replace the hand-written `TryFrom<HashMap<String, AttributeValue>>` for `ShortUrl` with `serde_dynamo`, eliminating the silent item-discard behaviour in `list_urls`.

**Acceptance Criteria:**
- AC-6.1: `shared/src/core.rs` no longer contains a manual `TryFrom<HashMap<String, AttributeValue>>` impl.
- AC-6.2: `serde_dynamo` is declared in `[workspace.dependencies]` and used in `shared`.
- AC-6.3: `list_urls` returns an error (propagated to the HTTP response) if any item fails deserialization, rather than silently omitting it.
- AC-6.4: All existing DynamoDB attributes (`LinkId`, `OriginalLink`, `Clicks`, `Title`, `Description`, `ContentType`, `Image`, `TimeStamp`, `SortKey`) round-trip correctly through serde_dynamo with no schema change.

---

### FR-7: Typed Error Handling with thiserror

Replace all `Result<T, String>` error patterns with typed error enums using `thiserror`, enabling the auth spec to map distinct error classes to distinct HTTP status codes.

**Acceptance Criteria:**
- AC-7.1: No function in the workspace returns `Result<T, String>`.
- AC-7.2: A dedicated error module (e.g. `shared/src/error.rs`) defines typed error variants covering: validation errors, DynamoDB operation failures, not-found, internal errors.
- AC-7.3: Lambda HTTP handlers map typed errors to appropriate HTTP status codes (400, 404, 500).
- AC-7.4: `cargo build --workspace` succeeds with no warnings.

---

### FR-8: Modernize Node.js / CDK Dependencies

Remove the stray `cdk` runtime dependency, update `aws-cdk-lib`, `aws-cdk`, `typescript`, and `@types/node` to current stable versions.

**Acceptance Criteria:**
- AC-8.1: `package.json` does not contain `"cdk"` in `dependencies`.
- AC-8.2: `aws-cdk-lib` is updated to ≥2.180.0.
- AC-8.3: `aws-cdk` (devDependency) is updated to match the installed CLI version (2.1136.0).
- AC-8.4: `typescript` is updated to ≥5.7.
- AC-8.5: `@types/node` is updated to ≥22.12.
- AC-8.6: `npm run build` succeeds and `cdk synth` produces valid CloudFormation.

---

### FR-9: Review and Adopt CDK Feature Flags

Audit the `cdk.json` context block against the flags recommended by the installed CDK version. Adopt new flags deliberately after verifying via `cdk diff` that no live resource is replaced.

**Acceptance Criteria:**
- AC-9.1: All flags recommended by CDK ≥2.180.0 are present in `cdk.json`.
- AC-9.2: `cdk diff` shows no unintended resource replacements from new flags.
- AC-9.3: Any flag that would cause a resource replacement is documented as a conscious exclusion with a comment.

---

### FR-10: Replace Placeholder CDK Tests

Replace the stock SQS Queue placeholder test with real `Template.fromStack` assertions covering the existing infrastructure.

**Acceptance Criteria:**
- AC-10.1: `test/krtk-rs.test.ts` contains assertions for: the DynamoDB table, at least one Lambda function, the CloudFront distribution, the API Gateway HTTP API, the S3 bucket, and the Kinesis stream.
- AC-10.2: `npm test` passes with all assertions green.
- AC-10.3: No commented-out code remains in the test file.

---

### FR-11: Private S3 Bucket with CloudFront Origin Access Control

Convert the hosting bucket from world-readable (publicReadAccess + disabled block-public-access) to a private bucket accessible only through CloudFront Origin Access Control.

**Acceptance Criteria:**
- AC-11.1: The bucket has `publicReadAccess: false` (or omitted) and the default `BlockPublicAccess.BLOCK_ALL`.
- AC-11.2: An OAC is configured on the CloudFront distribution for the S3 origin.
- AC-11.3: The bucket policy grants `s3:GetObject` only to the CloudFront distribution's OAC principal.
- AC-11.4: `defaultRootObject` is set on the distribution so `https://krtk.rs/` serves `index.html`.
- AC-11.5: The existing `errorResponses` 404→`/` mapping continues to work (SPA fallback).
- AC-11.6: The `/terms` and `/privacy` extensionless paths continue to serve HTML with the correct `content-type` header.
- AC-11.7: The `/assets/*` path pattern continues to serve static assets.
- AC-11.8: The bucket retains `removalPolicy: DESTROY` and `autoDeleteObjects: true` (reproducible assets).

---

### FR-12: DynamoDB Table Hardening

Change the link table from `DESTROY` removal policy to `RETAIN` with point-in-time recovery and deletion protection enabled.

**Acceptance Criteria:**
- AC-12.1: `linkTable` has `removalPolicy: cdk.RemovalPolicy.RETAIN`.
- AC-12.2: Point-in-time recovery is enabled on the table.
- AC-12.3: Deletion protection is enabled on the table.
- AC-12.4: `cdk diff` confirms the change is an in-place update, not a replacement.

---

### FR-13: ARM64 / Graviton Lambda Architecture

Move all Lambda functions from the default x86_64 to arm64 (Graviton), with `cargo-lambda` targeting `aarch64-unknown-linux-gnu`.

**Acceptance Criteria:**
- AC-13.1: All `RustFunction` constructs specify `architecture: Architecture.ARM_64`.
- AC-13.2: `cargo-lambda` build commands target `aarch64-unknown-linux-gnu`.
- AC-13.3: All Lambdas deploy and execute successfully on arm64.

---

### FR-14: Explicit Log Groups with Retention

Give every Lambda function an explicit CloudWatch log group with defined retention and JSON-structured logging, replacing the implicit never-expire groups.

**Acceptance Criteria:**
- AC-14.1: Every Lambda (`createLink`, `getLinks`, `visitLink`, `processAnalytics`) has an explicit `LogGroup` construct with retention set (1 week for all, matching the existing `processAnalytics` group).
- AC-14.2: No Lambda relies on an implicitly-created log group.
- AC-14.3: Lambda logging uses `tracing` with JSON formatting (already partially in place; verify consistency across all handlers).

---

## Non-Functional Requirements

### NFR-1: Zero Behaviour Change

The deployed application must behave identically after all changes:
- Shortening a URL produces a working redirect.
- Listing links returns all existing links in timestamp-descending order with pagination.
- Following a short link redirects correctly and the analytics click count increments.
- The website renders identically in a browser.

### NFR-2: Deployment Sequencing

Infrastructure-changing items (FR-11 through FR-14) must deploy in a separate CDK deployment from the dependency/refactor items (FR-1 through FR-10), enabling independent rollback.

### NFR-3: Verification by Execution

Each requirement is verified through real execution — not just compilation:
- `cargo build --workspace`
- `cargo tree` confirming single dependency versions
- `npm test` passing
- `cdk diff` showing expected changes
- A deploy exercised by hand: shorten a link, list links, follow a redirect, confirm analytics increment.

---

## Out of Scope

The following belong to the `i-want-to-introduce-cognito` spec and are explicitly excluded:
- Cognito User Pool or any authentication mechanism
- Per-user link ownership / `OwnerId` attribute
- API keys for M2M access
- CORS origin restriction (currently `allowOrigins: ['*']`)
- Custom Cognito domain (`auth.krtk.rs`)

---

## Environment Assumptions

| Resource | Value |
|---|---|
| Rust toolchain | cargo/rustc 1.97.1 at `~/.cargo/bin` |
| Node.js | 24.19.0 |
| CDK CLI | 2.1136.0 |
| AWS Account | 503716878456 |
| Main stack region | us-west-2 |
| Certificate stack region | us-east-1 |
| AWS credentials | Configured locally |
