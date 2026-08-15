# Tasks: Cognito Authentication for krtk.rs

Implements `design.md` against `requirements.md`. Base: `main` at `9ae11fd` (post-PR #9).

**Conventions** (from the `modernize-and-harden` spec): work in a git worktree on a `spec/` branch, one flat commit, PR to `darko-mesaros/krtk.rs`. Verification is real execution — `cargo build --workspace`, `cargo clippy`, `cargo test`, `npm test`, `cdk diff` — not review.

Legend: **[code]** agent-implementable · **[deploy]** requires AWS write · **[human]** requires Darko (console/TOTP device/decision)

---

## ⚠ Read before executing Phase 7

Sequencing `design.md` §8 exposed a transient the design did not name. The GSI partition key and the query that reads it **must flip together**:

- Before: every item has `SortKey = "LINKS"`; `get_links` queries `:pk = "LINKS"`.
- After: every item has `SortKey = "USER#<sub>"`; `get_links` queries `:pk = "USER#<sub>"`.

Whichever happens first, there is a window where the list renders **empty** — migrate first and the old code queries `"LINKS"` and finds nothing; deploy first and the new code queries `USER#<sub>` before the data says so. There is no ordering that avoids it, because it is a simultaneous data-and-code cutover.

This is acceptable and bounded, but must be done knowingly:

- **Redirects are never affected.** `visit_link` and `process_analytics` key on `LinkId` (the table partition key), which never changes. No short URL breaks at any point (FR-7.4).
- Only the *listing* is affected, for the minutes between task 30 and task 31.
- The table is small and single-owner, so the migration completes in seconds.
- **Do tasks 30 and 31 back to back in one window.** Do not stop between them.

An empty list during that window is expected. It is not data loss — the items are present and resolvable throughout.

---

## Phase 1 — Shared crate foundations
No behaviour change. Compiles and passes on its own.

- [x] **1.** **[code]** Add `Unauthorized` (401) and `Forbidden` (403) to the `thiserror` enum in `shared/src/error.rs` and extend the status-code mapping. Messages stay opaque and name no internal service, consistent with `Database` rendering as "Data store operation failed". *(FR-2.4, design §3.3)*
  - Verify: unit test asserting both map to the right status.
- [x] **2.** **[code]** Replace the `SORT_KEY_VALUE` constant in `shared/src/core.rs` with a documented `owner_key(sub: &str) -> String` helper returning `USER#<sub>`. No call site builds the value by hand. *(design §2.2)*
  - Verify: unit test on the returned format.
- [x] **3.** **[code]** Add `OwnerId` to `ShortUrlRow` as `Option<String>` (pre-migration rows must still deserialize, per FR-7.6) and `SortKey` with a comment stating it holds `USER#<sub>` and is the **GSI partition key, not a sort key**. Leave `ShortUrl` (wire struct) untouched. *(FR-3.1, FR-3.1a, design §3.2)*
  - Verify: deserialization tests **with and without** `OwnerId`.
- [x] **4.** **[code]** Add `owner_from_request(&Request) -> Result<String, Error>` to `shared`, handling **both** authorizer context shapes: `requestContext.authorizer.lambda.ownerId` and `requestContext.authorizer.jwt.claims.sub`, returning `Unauthorized` when neither is present. No handler reads the request context directly. *(design §4 trap)*
  - Verify: tests for both shapes **and** the absent case. This is the one bug that passes a links-only test suite and fails every `/api/keys` call.

## Phase 2 — Infrastructure: Cognito and the key table
Deployable with **no enforcement**. The site behaves exactly as it does today.

- [x] **5.** **[code]** Add an ACM certificate for `auth.krtk.rs` to `lib/certificate-stack.ts` (already `us-east-1`, which Cognito requires regardless of pool region), DNS-validated against the existing hosted zone. Export its ARN. *(FR-8.2, FR-8.3)*
- [x] **6.** **[code]** Add the Cognito user pool to `lib/krtk-rs-stack.ts`: email as sign-in alias, **MFA required, TOTP only, SMS disabled**, self-registration disabled (`AllowAdminCreateUserOnly`), `deletionProtection: true`, **Essentials** feature plan. *(FR-1.2, FR-1.3, FR-1.4, design §2.5, §10.2)*
  - Note: do **not** select the Plus tier — it has no free tier and bills from the first user (§10.2).
- [x] **7.** **[code]** Add the app client: **public, no secret**, authorization code grant with PKCE, **implicit grant disabled**, access/ID tokens 1h, refresh 30d, **refresh token rotation enabled** (this is what requires Essentials), callback `https://krtk.rs/auth/callback`, sign-out `https://krtk.rs/`. *(FR-1.5–FR-1.8)*
- [x] **8.** **[code]** Add the Cognito custom domain `auth.krtk.rs` using the task-5 certificate, plus a Route53 alias record to the distribution Cognito provisions. *(FR-8.1, FR-8.4)*
- [x] **9.** **[code]** Add `apiKeyTable`: PK `KeyHash`, GSI `OwnerIndex` (PK `OwnerId`, SK `CreatedAt`, projection ALL), `RETAIN`, `deletionProtection: true`, **PITR deliberately OFF** (restoring would resurrect revoked keys — §2.3), TTL on `ExpiresAt` for self-cleaning only. *(FR-4.9, design §2.3)*
  - Add a comment explaining the PITR asymmetry with `linkTable` so it is not "fixed" later.
- [x] **10.** **[code]** CDK assertions: `MfaConfiguration: ON` with TOTP only and SMS absent; `AllowAdminCreateUserOnly: true`; client has no secret, allows the code grant, does **not** allow implicit; `apiKeyTable` has PITR disabled and deletion protection enabled. *(design §9)*
- [x] **11.** **[code]** `npm test` and `cargo build --workspace` green; `cdk diff` reviewed and shows only additive resources.

## Phase 3 — Authorizer and key management
- [x] **12.** **[code]** New workspace member `lambda/authorizer` (arm64, `provided.al2023`, matching existing functions). JWT path: verify RS256 against the pool JWKS, checking `iss`, **`token_use == "access"`** (an ID token must be rejected), `client_id`, `exp`. Cache JWKS in warm memory with a 1-hour refresh and a re-fetch on unknown `kid`. *(design §4)*
  - Verify: unit tests for `token_use` rejection and expiry.
- [x] **13.** **[code]** Authorizer API key path: SHA-256 the presented `X-Api-Key`, `GetItem` on `KeyHash`, reject when absent or `ExpiresAt` has passed (checked in code — **never** relying on TTL having fired), bump `LastUsedAt` best-effort only when older than an hour. Return `{ isAuthorized, ownerId }` plus `authMethod` for logging only. *(FR-4.2, FR-4.6, FR-4.8, design §2.3, §4)*
- [x] **14.** **[code]** New workspace member `lambda/manage_keys`: `POST` mint (`krtk_` + 43 base64url chars from a CSPRNG, store SHA-256 hash + 12-char `KeyPrefix`, return plaintext **once**), `GET` list (metadata only, never the secret), `DELETE /{keyId}` revoke. Enforce the 10-active-key cap via an `OwnerIndex` query. *(FR-4.1, FR-4.5–FR-4.7)*
  - Verify: unit tests for key shape, prefix extraction, hash determinism, cap enforcement.
- [x] **15.** **[code]** Wire the authorizers in CDK: `HttpLambdaAuthorizer` (REQUEST, **`resultsCacheTtl: 0`**) on `POST`/`GET /api/links`; native `HttpUserPoolAuthorizer` on `ANY /api/keys*`; **no authorizer** on `GET /{linkId}`. *(FR-2.1–FR-2.3, FR-4.3, FR-4.4, design §2.1)*
  - The cache TTL of zero is required by FR-4.6 — a revoked key must fail on its *next* request.
- [x] **16.** **[code]** Restrict CORS: replace `allowOrigins: ['*']` with the krtk.rs origin and add `Authorization` and `X-Api-Key` to `allowHeaders`. *(NFR-1)*
- [x] **17.** **[code]** CDK assertion that `/api/links` carries the Lambda authorizer and `/api/keys` carries the **user pool** authorizer — this asserts the FR-4.4 boundary structurally, so a refactor cannot silently let an API key reach key management. *(design §9)*

## Phase 4 — Ownership in the handlers
- [x] **18.** **[code]** `create_link`: take the owner from `owner_from_request`, write `OwnerId` and `SortKey = owner_key(sub)`. Never accept an owner from the body, a header, or a query parameter. *(FR-3.2, FR-3.6)*
- [x] **19.** **[code]** `get_links`: bind the `TimeStampIndex` query's `:pk` to `owner_key(sub)` instead of the old constant. Pagination semantics unchanged — `ExclusiveStartKey` already carries `SortKey`, `LinkId`, `TimeStamp`. *(FR-3.3, FR-2.2)*
- [x] **20.** **[code]** Confirm `visit_link` and `process_analytics` are **unchanged**, and add a test asserting the redirect path requires no authorizer. *(FR-2.3, FR-6.1, FR-6.2)*
- [x] **21.** **[code]** Add a comment on the CDK `TimeStampIndex` declaration noting it now partitions per owner. *(design §2.2 naming debt)*

## Phase 5 — Frontend
- [x] **22.** **[code]** `website/assets/auth.js`: PKCE challenge/verifier generation, code exchange, token store in **`sessionStorage`**, refresh when expired or within 5 minutes of expiry, single-retry-then-redirect on 401. Verifier is single-use and deleted after exchange. *(FR-5.2, FR-5.4, FR-5.5, FR-5.8)*
- [x] **23.** **[code]** `website/auth/callback` — **extensionless** object, matching the existing `/terms` and `/privacy` pattern. Exchanges the code, then redirects to `/`. Must carry its own copy of the theme boot script (task 26) or it flashes white mid-redirect. *(FR-5.2, FR-5.10)*
- [x] **24.** **[code]** CloudFront: add an `/auth/*` behaviour → S3 origin with a `ResponseHeadersPolicy` forcing `content-type: text/html; charset=utf-8`, **ordered above `/?*`**. Without this, `/auth/callback` matches `/?*` (`?` matches one character) and is handed to `visit_link` as a short-link lookup — the same collision `/index.html` hit during the OAC migration. *(FR-6.3, design §2.4)*
  - Verify: CDK assertion that the behaviour exists and precedes `/?*`.
- [x] **25.** **[code]** `index.html`: redirect to the Hosted UI when unauthenticated (no shortening UI shown first), signed-in header with email, sign-out control that clears `sessionStorage` **and** calls Cognito's logout endpoint. *(FR-5.1, FR-5.6)*
  - Sign-out that only clears local tokens leaves the Cognito session alive, so the next login silently skips password and TOTP — indistinguishable from broken 2FA.
- [x] **26.** **[code]** Absorb the FR-9 dark mode work from PR #8: `tailwind.config = { darkMode: 'class' }`; inline boot script **above** the Tailwind/HTMX tags reading `localStorage.theme` with a `prefers-color-scheme` fallback, `try`/`catch` failing open to light; `#theme-toggle` with `aria-pressed`; toggle logic in `main.js`; `dark:` variants across `links_table.html`, `new_short_link.html`, `error_popup.html`; `.dark .spinner` base ring `#4b5563` keeping the `#008000` accent; `@media (prefers-color-scheme: dark)` on `website/privacy` and `website/terms`. *(FR-9.1–FR-9.8)*
  - The boot script's position is the mechanism — moving it into `main.js` looks tidier and reintroduces the theme flash.
- [x] **27.** **[code]** `.gitignore`: ensure **both** `*.log` and `.agents/` are ignored. PR #8 replaces the former with the latter because it branched before the modernization merge; carrying it verbatim would start tracking deploy transcripts. *(FR-9.9)*
- [x] **28.** **[code]** API key management UI in `index.html`: mint with label + optional expiry, list keys (metadata only), revoke. Newly minted key shown **once** with an explicit "will not be shown again" warning. *(FR-4.5, FR-5.7)*
- [x] **29.** **[code]** Generate `website/assets/auth-config.js` at deploy time via `Source.data()` on the existing `BucketDeployment`, so pool ID, client ID, and auth domain resolve from CDK tokens rather than being hardcoded or hand-edited. *(design §6)*
- [x] **30.** **[code]** `tools/migrate_owners` bin crate: paginated `Scan` projecting `LinkId, OwnerId`; for each item lacking `OwnerId`, `UpdateItem SET OwnerId, SortKey` with `ConditionExpression: attribute_not_exists(OwnerId)`. Takes `--table`, `--owner-sub`, `--dry-run`. Reports inspected / updated / skipped. *(FR-7.1, FR-7.3, FR-7.5, FR-7.7, design §7)*
  - Idempotency is structural via the condition expression, not via bookkeeping. A second run is a no-op by construction.
- [x] **31.** **[code]** Full local gate green: `cargo build --workspace`, `cargo clippy`, `cargo test`, `cargo audit`, `npm test`, `cdk diff` reviewed.

## Phase 6 — Deploy (ordered; do not reorder)
- [ ] **32.** **[deploy]** Deploy `CertificateStack` (auth cert). Wait for DNS validation to issue.
- [ ] **33.** **[deploy]** Deploy the pool, app client, custom domain, and `apiKeyTable`. **No enforcement yet** — the site still behaves as today.
- [ ] **34.** **[human]** **Verify `https://auth.krtk.rs` actually serves the login page.** Cognito provisions its own CloudFront distribution and this lags `cdk deploy` returning success. A "deploy succeeded but login 404s" state is otherwise very confusing. *(FR-8.6)*
- [ ] **35.** **[human]** `admin-create-user` for Darko. Complete the forced password change and **enrol TOTP** in an authenticator app. Capture the user's `sub`.
- [ ] **36.** **[human]** `migrate_owners --dry-run --owner-sub <sub>`. Inspect the reported counts and confirm they match the expected link count. The condition expression means a run against the **wrong** `sub` cannot be repaired by re-running — the dry run is the actual safeguard, not a formality. *(design §11)*
- [ ] **37.** **[deploy]** Run the migration for real. Confirm reported updated count matches the dry run.
- [ ] **38.** **[deploy]** **Immediately** deploy the authorizer, `manage_keys`, handler changes, routes, CORS, CloudFront behaviour, and frontend. Enforcement begins here. Do not pause between 37 and 38 — see the sequencing note at the top.

## Phase 7 — Verify
- [ ] **39.** **[human]** Login end to end: redirected to `auth.krtk.rs`, password + TOTP required, returned authenticated. *(criteria 2, 3, 4, 8)*
- [ ] **40.** **[human]** Migrated links appear with original click counts and timestamps; every pre-existing short URL still resolves. *(criteria 16, 17)*
- [ ] **41.** **[human]** Re-run the migration — reports zero updates, changes nothing. *(criterion 18)*
- [ ] **42.** **[human]** Unauthenticated `POST`/`GET /api/links` return 401; `GET /{linkId}` still redirects for anyone. *(criteria 1, 11)*
- [ ] **43.** **[human]** Mint a key, use it from the CLI to create and list links, revoke it, confirm the next request is 401. Confirm `/api/keys` rejects `X-Api-Key`. *(criteria 12, 13, 14)*
- [ ] **44.** **[human]** Sign out, then confirm returning to krtk.rs requires **full** re-authentication including TOTP — not a silent Hosted UI resume. *(criterion 6)*
- [ ] **45.** **[human]** Both themes render correctly on every auth surface including the callback page, with no white flash mid-redirect; sign-out preserves the theme. *(criteria 20, 21, 26, 27)*

## Phase 8 — Close out
- [ ] **46.** **[code]** Open the PR. Read the Infracost comment: a near-zero diff with Cognito absent is the **expected** result (§10.5). A non-trivial fixed monthly increase means something priced-and-always-on crept in.
- [ ] **47.** **[human]** Close PR #8 with a reference to the commit that supersedes it. Do not merge it. *(criterion 29)*
- [ ] **48.** **[code]** *(optional)* Add `infracost-usage.yml` with estimated monthly Lambda invocations, DynamoDB requests, and CloudFront requests, so usage-based line items produce real numbers and the §10.4 doubled-invocation effect can be quantified. *(design §10.5)*
