# Tasks: Modernize and Harden the krtk.rs Codebase

## Phase A: Code & Dependencies (behaviour-preserving)

### Task 1: Verify Build Toolchain [FR-1]

- [x] 1.1: Run `cargo lambda --version` to confirm cargo-lambda is installed.
- [x] 1.2: Run `cargo lambda build --release --arm64 --workspace` and confirm all 4 binaries build successfully.
- [x] 1.3: Run `cdk synth` and confirm no errors related to missing build tools.

### Task 2: Fix Workspace Manifest [FR-2]

- [x] 2.1: Remove the duplicate `lambda/create_link` entry from `[workspace] members` in the root `Cargo.toml`.
- [x] 2.2: Add a `[workspace.dependencies]` section to the root `Cargo.toml` with shared dependencies: `lambda_http`, `lambda_runtime`, `tokio`, `aws-config`, `aws-sdk-dynamodb`, `aws-sdk-secretsmanager`, `serde`, `serde_json`, `aws_lambda_events`, `reqwest`, `chrono`, `thiserror`, `serde_dynamo`, `tracing`, `tracing-subscriber`.
- [x] 2.3: Consolidate `lambda_http` to version 0.14 (the version currently in `shared`). Remove the 0.13.0 pins from all Lambda member `Cargo.toml` files.
- [x] 2.4: Update `shared/Cargo.toml` to use `dep.workspace = true` for all shared dependencies.
- [x] 2.5: Update `lambda/create_link/Cargo.toml` to use `dep.workspace = true` for shared dependencies.
- [x] 2.6: Update `lambda/get_links/Cargo.toml` to use `dep.workspace = true` for shared dependencies.
- [x] 2.7: Update `lambda/visit_link/Cargo.toml` to use `dep.workspace = true` for shared dependencies.
- [x] 2.8: Update `lambda/process_analytics/Cargo.toml` to use `dep.workspace = true` for shared dependencies.
- [x] 2.9: Fix any import path changes required by lambda_http 0.14 in `create_link/src/main.rs`, `get_links/src/main.rs`, and `visit_link/src/main.rs`.
- [x] 2.10: Run `cargo build --workspace` — must succeed with zero dependency resolution warnings.
- [x] 2.11: Run `cargo tree --workspace -i lambda_http` — must show exactly one version.

### Task 3: Pin Rust Toolchain and Upgrade to Edition 2024 [FR-3]

- [x] 3.1: Create `rust-toolchain.toml` at the workspace root with `channel = "stable"` and `targets = ["aarch64-unknown-linux-gnu"]`.
- [x] 3.2: Run `cargo fix --edition` across the workspace to auto-apply edition 2024 required changes.
- [x] 3.3: Update `edition = "2021"` to `edition = "2024"` in all 5 `Cargo.toml` files (root workspace has no edition; shared + 4 lambdas do).
- [x] 3.4: Run `cargo build --workspace` — must succeed under edition 2024 semantics.
- [x] 3.5: Run `cargo clippy --workspace` — fix any new lints introduced by edition 2024.

### Task 4: Migrate Askama to Rinja [FR-4]

- [x] 4.1: Replace `askama = { version = "0.12.1", features = ["serde_json"] }` with `rinja` (latest stable) in `shared/Cargo.toml` (via workspace dependency).
- [x] 4.2: Update `shared/src/templates.rs`: change `pub use askama::{Template, Error}` to `pub use rinja::{Template, Error}`.
- [x] 4.3: Update the `filters` module if any import paths changed between askama and rinja.
- [x] 4.4: Run `cargo build --workspace` — all templates must compile.
- [x] 4.5: Manually inspect rendered output of `LinksTable`, `NewShortLink`, and `ErrorPopup` templates to confirm byte-identical HTML output (or write a unit test comparing outputs). *(5 render tests added in `shared/src/templates.rs`, including an auto-escaping regression test)*

### Task 5: Pin AWS SDK Behaviour Version [FR-5]

- [x] 5.1: Remove the `behavior-version-latest` feature from `aws-config` in `[workspace.dependencies]`.
- [x] 5.2: In `lambda/create_link/src/main.rs`, replace `aws_config::load_defaults(aws_config::BehaviorVersion::latest())` with `aws_config::defaults(aws_config::BehaviorVersion::v2026_01_12()).load()`.
- [x] 5.3: Apply the same change in `lambda/get_links/src/main.rs`.
- [x] 5.4: Apply the same change in `lambda/visit_link/src/main.rs`.
- [x] 5.5: Apply the same change in `lambda/process_analytics/src/main.rs`.
- [x] 5.6: Run `cargo build --workspace` — must succeed.

### Task 6: Update All Rust Dependencies [FR-5b]

- [x] 6.1: Run `cargo update` to pull latest semver-compatible versions into `Cargo.lock`.
- [x] 6.2: Run `cargo outdated` (install if needed) to identify crates with major-version bumps available. *(used `cargo update --dry-run --verbose`; cargo-outdated not needed)*
- [x] 6.3: Evaluate each major-version candidate; bump pins in `[workspace.dependencies]` where migration is straightforward.
- [x] 6.4: Adapt code to any breaking API changes in updated crates.
- [x] 6.5: Run `cargo build --workspace` — must succeed.
- [x] 6.6: Run `cargo clippy --workspace -- -D warnings` — must produce zero warnings.
- [x] 6.7: Run `cargo audit` — must report no known vulnerabilities (install `cargo-audit` if needed). *(clean: 0 vulnerabilities, 0 warnings)*

**Major versions adopted (all built with zero code changes unless noted):**

| Crate | From | To | Notes |
|---|---|---|---|
| `lambda_http` | 0.14.0 | **1.3.0** | AWS Lambda Rust runtime reached 1.0. No code changes. |
| `lambda_runtime` | 0.13.0 | **1.3.0** | Paired with the above. No code changes. |
| `aws_lambda_events` | 0.15.1 | **1.2.0** | Paired with `lambda_runtime` 1.x. No code changes. |
| `reqwest` | 0.12.28 | **0.13.4** | Feature rename: `rustls-tls` → `rustls`; root store split into its own feature. Added `webpki-roots` to preserve 0.12 behaviour. |
| `scraper` | 0.22.0 | **0.27.0** | No code changes. Also dropped the unmaintained `fxhash` transitive (RUSTSEC-2025-0057). |
| `askama` | 0.12.1 | **0.16.0** | See Task 4. |
| `aws-config` | 1.5.15 | **1.10.1** | Unlocked `BehaviorVersion::v2026_01_12`. |

**Security finding — 3 CVEs closed (same bug class as the original dual-`lambda_http` finding):**

`cargo audit` initially reported RUSTSEC-2026-0098, -0099 and -0104 against `rustls-webpki 0.101.7`. Traced to `aws-sdk-dynamodb` and `aws-sdk-secretsmanager`, whose **default** feature set includes `rustls` → `aws-smithy-runtime/tls-rustls` → `aws-smithy-http-client/legacy-rustls-ring` → the legacy `rustls 0.21` + `hyper 0.14` stack. Every Lambda binary was compiling **two** TLS stacks and **two** hyper versions, and only the legacy one was vulnerable — while `BehaviorVersion::v2026_01_12` uses the modern client at runtime, so the legacy stack was pure dead weight.

Fixed by setting `default-features = false` on both SDK crates and re-enabling only `default-https-client` + `rt-tokio`. Result:
- `rustls`: 2 versions (0.21.12 + 0.23.x) → **1** (0.23.43)
- `hyper`: 2 versions (0.14.32 + 1.7.0) → **1** (1.11.0)
- `cargo audit`: 3 vulnerabilities → **0**
- Smaller binaries and faster cold starts as a side effect.

### Task 7: Adopt serde_dynamo [FR-6]

- [x] 7.1: Add `serde_dynamo` to `[workspace.dependencies]` with feature `aws-sdk-dynamodb+1`.
- [x] 7.2: Add `#[derive(Deserialize)]` to `ShortUrl` in `shared/src/core.rs` with `#[serde(rename = "...")]` attributes matching DynamoDB attribute names (`LinkId`, `OriginalLink`, `Clicks`, `Title`, `Description`, `ContentType`, `Image`, `TimeStamp`).
- [x] 7.3: Remove the manual `impl TryFrom<HashMap<String, AttributeValue>> for ShortUrl` block.
- [x] 7.4: Replace the for-loop in `list_urls` that silently discards failed items with `serde_dynamo::from_items(items)?` that propagates errors.
- [x] 7.5: Run `cargo build --workspace` — must succeed.
- [x] 7.6: Test that existing DynamoDB items deserialize correctly (manual verification via `cargo lambda invoke` or integration test). *(8 unit tests in `shared/src/core.rs` — no deployed table needed)*

The tests build the exact item shape `shorten_url` writes, so they fail if the writer and the
`#[serde(rename)]` attributes ever drift apart. Beyond the happy path they cover:
- **Absent optional attributes.** `shorten_url` omits `Title`/`Description`/`ContentType`/`Image`
  entirely when scraping yields nothing, rather than writing nulls.
- **Unknown attributes are ignored.** The auth spec adds `OwnerId` to this table; if reads
  rejected unknown attributes, deploying the writer first would break link listing.
- **A missing or wrongly-typed required attribute now errors.** This is the regression guard
  for the actual bug: `list_urls` used to discard malformed items, so a mapping error
  presented as links silently vanishing from the list instead of as a failure.

### Task 8: Typed Error Handling [FR-7]

- [x] 8.1: Create `shared/src/error.rs` with `AppError` enum using `thiserror` (variants: `Validation`, `NotFound`, `Database`, `Serialization`, `SafeBrowsing`, `Template`, `Internal`). User-facing messages must NOT expose internal service names.
- [x] 8.2: Add `pub mod error;` to `shared/src/lib.rs`.
- [x] 8.3: Implement `status_code()` method on `AppError` mapping variants to HTTP status codes (400, 404, 500).
- [x] 8.4: Refactor `UrlShortener` methods in `shared/src/core.rs` to return `Result<T, AppError>` instead of `Result<T, String>`.
- [x] 8.5: Refactor `ShortenUrlRequest::validate()` chain to return `Result<Self, AppError>`.
- [x] 8.6: Refactor `shared/src/safe_browsing.rs` to return `AppError` variants. *(also refactored `shared/src/url_info.rs`, the last remaining `Result<T, String>`)*
- [x] 8.7: Update `lambda/create_link/src/main.rs` handler to use typed errors and `AppError::status_code()` for responses.
- [x] 8.8: Update `lambda/get_links/src/main.rs` handler similarly.
- [x] 8.9: Update `lambda/visit_link/src/main.rs` handler similarly.
- [x] 8.10: Update `lambda/process_analytics/src/main.rs` handler similarly.
- [x] 8.11: Delete `lambda/visit_link/src/http_handler.rs` (unused stock cargo-lambda boilerplate).
- [x] 8.12: Run `cargo build --workspace` — must succeed with no `Result<T, String>` remaining.

### Task 9: Modernize Node.js / CDK Dependencies [FR-8]

- [x] 9.1: Run `npm uninstall cdk` to remove the stray runtime dependency.
- [x] 9.2: Run `npm install aws-cdk-lib@latest cargo-lambda-cdk@latest constructs@latest`.
- [x] 9.3: Run `npm install -D aws-cdk@2.1136.0 typescript@latest @types/node@latest @types/jest@latest`.
- [x] 9.4: Run `npm run build` (tsc) — must succeed.
- [x] 9.5: Run `cdk synth` — must produce valid CloudFormation.

### Task 10: Review and Adopt CDK Feature Flags [FR-9]

- [x] 10.1: Run `cdk context --clear` to remove cached context.
- [x] 10.2: Compare existing `cdk.json` flags against flags recommended by CDK 2.180+ (check CDK source / `cdk doctor`).
- [x] 10.3: Add any new recommended flags to `cdk.json`. *(23 of 25 adopted; unconfigured count 25 → 2)*
- [x] 10.4: Run `cdk diff` — verify no unintended resource replacements. *(no replacements possible — stacks are NOT yet deployed; every resource is `[+]`)*
- [x] 10.5: Document any excluded flags (that would cause replacement) with `// EXCLUDED:` comments in `cdk.json`.

**Deliberately deferred flags** (adopted in Phase B where they belong — `cdk.json` is strict JSON so exclusions are documented here, not inline):
- `@aws-cdk/aws-s3:publicAccessBlockedByDefault` — does part of Task 13's job (private bucket). Adopt with Task 13.
- `@aws-cdk/aws-lambda:useCdkManagedLogGroup` — does part of Task 16's job (explicit log groups). Adopt with Task 16.
- `@aws-cdk/core:defaultCrossStackReferences` set to `"both"`, not the recommended `"weak"`: the documented migration is both → deploy everywhere → weak. Flip to `weak` only after the first successful deploy of all three stacks.

### Task 11: Replace Placeholder CDK Tests [FR-10]

- [x] 11.1: Rewrite `test/krtk-rs.test.ts` with `Template.fromStack` assertions covering: DynamoDB table (partition key, GSI), Lambda functions (runtime, architecture), CloudFront distribution (domain, behaviours), HTTP API (routes), S3 bucket, Kinesis stream.
- [x] 11.2: Ensure test setup correctly instantiates `KrtkRsStack` with required props (`certificateArn`, `googleApiKeySecret`).
- [x] 11.3: Remove all commented-out code from the test file.
- [x] 11.4: Run `npm test` — all assertions must pass. *(23/23 passing)*

**TypeScript version finding (affects AC-8.4):** `npm install typescript@latest` resolves to **TypeScript 7.0.2**, the native (Go) rewrite, which no longer exposes the JavaScript compiler API. That broke BOTH `ts-node` (cdk app runner) and `ts-jest` (test transformer) with the same `Cannot read properties of undefined (reading 'fileExists')` signature. TypeScript is therefore pinned to **6.0.3** — the last release with the JS compiler API, and still comfortably above the AC-8.4 floor of ≥5.7. Migrating to TS 7 requires `@typescript/native` plus a ts-jest alias and is out of scope for a behaviour-preserving spec; it belongs in its own follow-up.

### Task 12: Phase A Verification

- [x] 12.1: Run `cargo build --workspace` — clean build.
- [x] 12.2: Run `cargo tree --workspace -i lambda_http` — single version. *(1.3.0; also verified single `rustls` and single `hyper`)*
- [x] 12.3: Run `npm test` — green. *(23/23; plus 5 Rust render tests via `cargo test`)*
- [x] 12.4: Run `cdk diff` — expect only Lambda code asset hash changes (no infrastructure changes).
- [x] 12.5: ~~Run `cdk deploy --all`~~ — **superseded by Task 18.4.** Phase B was landed before the first deploy, so there is only one deploy, not two.
- [x] 12.6: ~~Manual smoke test~~ — **superseded by Task 18.5.**

> **BLOCKER — deployment sequencing needs a decision.**
>
> `cdk diff` revealed that **none of the three stacks is currently deployed** in account
> 503716878456 — every resource in `CertificateStack`, `SecretsStack` and `KrtkRsStack` shows
> as `[+]` (create), with zero `[~]` modifications and zero replacements. There is no live
> site and no existing DynamoDB data.
>
> That invalidates the premise NFR-2 rests on. The Phase A / Phase B split existed so
> infrastructure changes could be rolled back independently of code changes against a
> **running** stack. With nothing deployed, deploying Phase A first would stand up a
> world-readable S3 bucket and a `DESTROY`-policy table, and only then harden them in Phase B —
> strictly worse than deploying the hardened configuration once.
>
> Consequences if a single combined deployment is chosen:
> - NFR-1 ("zero behaviour change") becomes first-run validation rather than a regression check —
>   there is no before-state to compare against.
> - Task 18.3 (delete pre-existing implicit log groups) becomes unnecessary; there are none.
> - Tasks 12.4 / 14.4 / 18.1 / 18.2 ("confirm no replacements") are trivially satisfied.
>
> Also note: `cdk.json` pins `"profile": "personal"`, which does not exist on this host.
> AWS credentials resolve via the **default** profile (verified:
> `arn:aws:iam::503716878456:user/cli-access`). Every `cdk` invocation here needs
> `--profile default`, or that pin needs changing.

---

## Phase B: Infrastructure Hardening (separate deployment)

### Task 13: Private S3 Bucket with OAC [FR-11]

- [x] 13.1: In `lib/krtk-rs-stack.ts`, remove `publicReadAccess: true` and the `blockPublicAccess` override from the hosting bucket. *(now `BlockPublicAccess.BLOCK_ALL`; also added `enforceSSL: true`)*
- [x] 13.2: Remove `websiteIndexDocument: 'index.html'` from the bucket (not needed with OAC).
- [x] 13.3: Replace `S3StaticWebsiteOrigin` with `S3BucketOrigin.withOriginAccessControl(hostingBucket)` for all S3 origin behaviours (default, `/assets/*`, `/terms`, `/privacy`). *(one shared `s3Origin` instance across all four)*
- [x] 13.4: Set `defaultRootObject: 'index.html'` on the CloudFront distribution.
- [x] 13.5: Verify the existing `errorResponses` 404→`/` mapping is preserved (SPA fallback).
- [x] 13.6: Verify the `/terms` and `/privacy` behaviours still apply the `ResponseHeadersPolicy` for content-type.
- [x] 13.7: Keep `removalPolicy: DESTROY` and `autoDeleteObjects: true` on the bucket.
- [x] 13.8: Run `npm run build` — must succeed.

### Task 14: DynamoDB Table Hardening [FR-12]

- [x] 14.1: Change `removalPolicy: cdk.RemovalPolicy.DESTROY` to `cdk.RemovalPolicy.RETAIN` on `linkTable`.
- [x] 14.2: Add `pointInTimeRecovery: true` to `linkTable`. *(TableV2 uses `pointInTimeRecoverySpecification`; the flat prop is deprecated)*
- [x] 14.3: Add `deletionProtection: true` to `linkTable`.
- [x] 14.4: Run `cdk diff` — confirm in-place update, NOT a replacement. *(N/A — table does not exist yet; verified `DeletionPolicy: Retain` in the synthesized template instead)*

### Task 15: ARM64 / Graviton Lambda Architecture [FR-13]

- [x] 15.1: Add `import { Architecture } from 'aws-cdk-lib/aws-lambda'` to `krtk-rs-stack.ts`.
- [x] 15.2: Add `architecture: Architecture.ARM_64` to all four `RustFunction` constructs.
- [x] 15.3: Verify `rust-toolchain.toml` includes `aarch64-unknown-linux-gnu` target (done in Task 3).
- [x] 15.4: Run `npm run build` — must succeed. *(template confirms `Architectures: [arm64]` on all 4)*

### Task 16: Explicit Log Groups with Retention [FR-14]

- [x] 16.1: Create explicit `LogGroup` constructs for `createLinkLambda`, `getLinksLambda`, and `visitLinkLambda` with `retention: ONE_WEEK`, `removalPolicy: DESTROY`.
- [x] 16.2: Add `node.addDependency(lambda)` on each new log group. *(not needed — see note below)*
- [x] 16.3: Verify the existing `processAnalyticsLogGroup` already has these settings.
- [x] 16.4: Confirm all Lambda handlers emit JSON-structured logs. *(set declaratively via `loggingFormat: LoggingFormat.JSON`, which is stronger than relying on the runtime's default subscriber)*
- [x] 16.5: Run `npm run build` — must succeed.

**Implementation note (improves on the design):** the design carried forward the existing
`processAnalytics` pattern — create the log group *after* the function with
`logGroupName: /aws/lambda/${fn.functionName}`, then `addDependency()`. That couples the two
resources by name and needs an explicit dependency edge to avoid a race. Instead all four
groups are now declared *before* the functions and passed via the `logGroup` prop, so
CloudFormation derives the dependency from the reference itself. The `addDependency()` call
and the name-matching template string are both gone, and `processAnalytics`'s
`MetricFilter` binds to the same construct directly.

### Task 17: Update CDK Tests for Phase B [FR-10 addendum]

- [x] 17.1: Update `test/krtk-rs.test.ts` assertions to reflect: private bucket (no public access), OAC on distribution, ARM64 architecture on Lambdas, RETAIN on DynamoDB, explicit log groups.
- [x] 17.2: Run `npm test` — all assertions must pass. *(38/38)*

Added 15 hardening assertions, including three that go beyond the task list because they
guard the failure modes that actually matter:
- **No `Allow` statement may carry a wildcard principal.** A naive string check for `"AWS":"*"`
  fails here, because `enforceSSL` legitimately uses a wildcard principal on a *Deny*. The
  test parses statements and only inspects `Effect: Allow`.
- **The CloudFront grant must carry an `AWS:SourceArn` condition.** Without it, any
  CloudFront distribution in any AWS account could read the bucket through OAC.
- **No log group may sit on infinite retention**, asserted across every group in the template
  rather than just the four application ones.

### Task 18: Deployment and Verification (single combined deploy)

Darko's decision: since nothing was deployed, Phase B hardening was landed alongside Phase A
and the stacks are deployed **once**, already hardened. This supersedes the Task 12 deploy
step and NFR-2's two-deploy split — see the blocker note under Task 12.

- [x] 18.1: Run `cdk diff` — review the full change set. *(all resources `[+]` create; nothing pre-exists)*
- [x] 18.2: Confirm NO resource replacements in the diff (only in-place updates and additions). *(trivially satisfied — first deploy)*
- [x] 18.3: Delete existing implicit CloudWatch log groups. *(N/A — no functions have ever run, so no implicit groups exist)*
- [x] 18.4: Run `cdk deploy --all` — successful deployment. *(all three stacks ✅, 386s, zero failures)*
- [x] 18.5: Manual smoke test: shorten a URL → list links → follow redirect → verify analytics click count increments. *(all pass — `clicks` went 0 → 1, proving the CloudFront → Kinesis → process_analytics → DynamoDB loop)*
- [x] 18.6: Verify `https://krtk.rs/` serves the homepage (defaultRootObject working). *(200 text/html — **required a fix**, see below)*
- [x] 18.7: Verify `https://krtk.rs/terms` and `https://krtk.rs/privacy` serve HTML with correct content-type. *(both 200 `text/html; charset=utf-8`)*
- [x] 18.8: Verify CloudWatch logs appear in the new explicit log groups with JSON formatting. *(all 4 app functions → own explicit group, retention 7, `LogFormat: JSON`)*
- [x] 18.9: Verify Lambdas are running on ARM64. *(all 4 `arm64`)*

Also verified live: bucket public-access all four blocks `true` and direct S3 access returns
**403**; DynamoDB `DeletionProtectionEnabled: true` and PITR `ENABLED`.

---

## Two regressions found by the live smoke tests (both fixed and redeployed)

Neither was caught by the 38 CDK tests, because those assert **synthesized configuration**
while both bugs were in **runtime behaviour**. This is the argument for 18.5–18.9 existing at
all — a green template is not a working site.

### 1. Apex returned 404 — the OAC switch (FR-11)

`https://krtk.rs/` returned `404` with an empty body and an `apigw-requestid` header, meaning
API Gateway served it, not S3.

In a CloudFront path pattern `?` matches **exactly one character**, so the `/?*` link-redirect
behaviour matches *every* root-level path with at least one character — including
`/index.html`. Under the old `S3StaticWebsiteOrigin` that never mattered: the website endpoint
resolved the index document **at the origin**, so no CloudFront-level rewrite happened. OAC
uses the S3 REST endpoint, which cannot do that, so `defaultRootObject` must rewrite
`/` → `/index.html` — and that rewritten path fell through into `/?*` and was served by the
`visit_link` Lambda, which correctly 404s an unknown link id.

design.md predicted this risk and asserted `defaultRootObject` was sufficient mitigation. It
was not: the design never considered the interaction with the pre-existing `/?*` catch-all.

**Fix:** an explicit `/index.html` behaviour pointing at the S3 origin, ordered before `/?*`,
plus `errorResponses.responsePagePath` changed from `/` to `/index.html` (pointing the error
page at the bare root made the error-page fetch itself re-enter the same trap, which is why
the 404 body was empty).

**Tests added (4):** `/index.html` is routed to the S3 origin; `/index.html` is ordered before
`/?*`; the behaviour set matches exactly; the 404 fallback targets a concrete object. These
assert routing **precedence**, which the existing config-shape assertions structurally cannot see.

### 2. `/api/links` JSON contract broke — serde renames leaked (FR-6)

Field names came back as `LinkId` / `OriginalLink` / `Clicks` / `TimeStamp` instead of
`link_id` / `original_link` / `clicks` / `timestamp`, and the HTMX path — the one the real
frontend uses — returned **500**.

Task 7.2 added `#[serde(rename = "LinkId")]` etc. to `ShortUrl` so serde_dynamo could read the
DynamoDB attribute names. But serde renames apply to `Serialize` **and** `Deserialize`, and
`ShortUrl` is also the API response type. `get_links` serializes it and then deserializes into
`templates::Link` (which expects snake_case), so that round-trip failed → 500.

This directly violated the spec's own constraint that there be **no API contract change**.

**Fix:** split the two namings into two types — `ShortUrlRow` (PascalCase, `Deserialize` only,
used solely by `serde_dynamo::from_items`) and `ShortUrl` (snake_case, `Serialize`, the public
wire shape), with a `From<ShortUrlRow>` conversion.

**Tests added (2):** one pins the exact set of serialized JSON keys so a DynamoDB-motivated
rename can never silently leak into the API again; the other asserts a serialized `ShortUrl`
still deserializes into `templates::Link`, covering the round-trip that actually 500'd.

Final state: 15 Rust tests + 41 CDK tests green, clippy clean, `cargo audit` clean.

**Pre-deploy checklist for whoever runs 18.4:**
1. `cdk.json` pins `"profile": "personal"`, which does not exist on this host — pass
   `--profile default` or change the pin. Verified identity:
   `arn:aws:iam::503716878456:user/cli-access`.
2. `CertificateStack` (us-east-1) issues a **DNS-validated** ACM certificate. Validation
   records go into hosted zone `Z07540833AST0TH4M5W39`; the deploy will block until the
   certificate validates.
3. Deploy order is handled by the stack dependencies already declared in `bin/krtk-rs.ts`
   (`KrtkRsStack` depends on both `CertificateStack` and `SecretsStack`).
4. `SecretsStack` creates the Google API key secret **empty** — the Safe Browsing check
   returns `AppError::Internal` until a value is pushed into it. URL validation fails
   closed-but-permissive (`validate_safe_browsing` deliberately fails open), so shortening
   still works; populate the secret to actually enable Safe Browsing.
5. After a successful deploy of all three stacks, flip
   `@aws-cdk/core:defaultCrossStackReferences` from `"both"` to `"weak"` and redeploy — that
   is the documented end state of the migration.

### Follow-ups deliberately left out of this spec

- **TypeScript 7 migration.** Pinned at 6.0.3 because TS 7 (native rewrite) drops the JS
  compiler API that `ts-node` and `ts-jest` both need. Needs `@typescript/native` plus a
  ts-jest alias.
- **`npm audit` findings.** 13 advisories remain in the JS dev-dependency tree (2 low,
  3 moderate, 7 high, 1 critical). None reach production — CDK tooling is build-time only,
  and no JS ships to a runtime — but they deserve their own pass.
- **`cdk diff` before the very first deploy cannot prove behaviour preservation.** NFR-1's
  "confirm nothing changed" premise assumed a live stack. Post-deploy, re-running the
  smoke tests in 18.5–18.9 is what establishes the baseline.
