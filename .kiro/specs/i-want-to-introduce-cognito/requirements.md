# Requirements: Cognito Authentication for krtk.rs

## Overview

Introduce Amazon Cognito authentication to krtk.rs so that the URL shortener is no longer publicly writable. Each authenticated user owns their own set of shortened links. The system starts with a single user (Darko) and is invite-only, to prevent the spam and fraud the public version attracted.

## Motivation

The current system is fully public — anyone can create short links and all links share a single global namespace and a single shared page. This has led to spam and fraud. Adding authentication gates link creation behind verified users, ties each link to its owner, and enables per-user link management.

## Scope and Prerequisites

This spec covers **authentication and per-user ownership only**.

The dependency modernization and infrastructure hardening originally drafted here were moved to a separate prerequisite spec, `modernize-and-harden`. That spec is **complete, deployed, and merged to main** (PR #9, commit `f62e1ed`, merged 2026-08-14). This spec builds directly on it.

**Verified foundation this spec depends on** (checked against `main` at `9ae11fd`):

- **Single `lambda_http`** — consolidated to the 1.x line via `[workspace.dependencies]`, so shared request/response types are coherent across all four Lambdas.
- **Typed errors** — `shared/src/error.rs` defines a `thiserror` enum with a status-code mapping. It currently covers `Validation` → 400, `NotFound` → 404, and `Database` / `Serialization` / `Template` / `Internal` → 500. It has **no `Unauthorized` or `Forbidden` variant**: adding 401 and 403 to this enum and its status mapping is this spec's work, and the mechanism to do it cleanly now exists.
- **`serde_dynamo` mapping with a deliberate wire/persistence split** — `ShortUrl` is the outbound API shape (`Serialize`, snake_case) and `ShortUrlRow` is the persistence shape (`Deserialize`, `#[serde(rename)]` to PascalCase DynamoDB attributes). The split exists because a single struct broke the JSON API contract. Adding `OwnerId` therefore means adding it to `ShortUrlRow` and deciding **explicitly** whether it belongs on the wire — it must not be leaked into `ShortUrl` by reflex.
- **Private bucket + CloudFront OAC**, DynamoDB `RETAIN` + PITR + deletion protection, arm64 Lambdas, explicit log groups with JSON logging.
- **Working build and test suite** — `cargo-lambda` 1.9.1 verified, 15 Rust + 41 CDK tests green, 0 audit findings. `test/krtk-rs.test.ts` now holds real `Template.fromStack` assertions, so this spec has a regression baseline.

**Known routing precedent that directly informs FR-6.3.** The OAC migration hit exactly the collision class FR-6.3 describes: CloudFront's `/?*` pattern swallowed `/index.html` (`?` matches a single character), which required an explicit higher-precedence `/index.html` behaviour plus `responsePagePath` pointing at `/index.html`. The OAC REST endpoint cannot resolve index documents at the origin the way the website endpoint did. The OAuth callback path will need the same treatment, and the existing fix is the template for it.

**Open conflict to be aware of.** PR #8 (`feature/dark-mode`, "Add dark mode with a toggle switch to the frontend") is still open and modifies `website/index.html` and `website/assets/main.js` — the same files FR-5 rewrites. Whichever lands second absorbs the merge. This is a sequencing question, not a requirements change.

## Actors

- **Admin** (Darko): The sole initial user. Can create links, view their own links, mint API keys, and create further users.
- **Authenticated User**: A user created by the admin. Can create and manage their own links and API keys.
- **Anonymous Visitor**: Anyone who follows a shortened link. Redirects remain public; they cannot create or list links.

## Functional Requirements

### FR-1: User Authentication via Cognito

Authentication is delegated entirely to Cognito's **Hosted UI (managed login)**. krtk.rs ships no login form, no password field, and no MFA challenge UI of its own — the browser leaves the site to authenticate and returns with an authorization code.

- **FR-1.1**: A Cognito User Pool hosts all user identities. Sign-in happens on the Cognito-hosted login page served at `auth.krtk.rs`, not on a krtk.rs page.
- **FR-1.2**: Login requires email + password. Email is the sign-in alias; usernames are not exposed to the user.
- **FR-1.3**: MFA is **required** for every user, via TOTP (authenticator app) only. SMS MFA is not enabled. The Hosted UI presents TOTP enrolment (QR code + verification) on first sign-in and a TOTP challenge on every subsequent sign-in.
- **FR-1.4**: Self-registration is disabled at the User Pool level — the Hosted UI presents no "Sign up" link. Users are created only by the admin (`admin-create-user`), who hands over a temporary password out of band. A user created this way must set a permanent password and enrol TOTP before reaching the app.
- **FR-1.5**: The app client is a **public client with no secret**, using the OAuth 2.0 **authorization code grant with PKCE**. Implicit grant is disabled.
- **FR-1.6**: On success, Cognito redirects to a krtk.rs callback URL with an authorization code; the frontend exchanges that code for an ID token, access token, and refresh token at Cognito's token endpoint.
- **FR-1.7**: Callback and sign-out URLs are restricted to the krtk.rs origin. Cognito rejects a redirect to any other origin.
- **FR-1.8**: Token lifetimes: access and ID tokens 1 hour, refresh token 30 days. Refresh token rotation is enabled.

### FR-2: Protected API Endpoints

- **FR-2.1**: `POST /api/links` (create link) requires a valid access token in the `Authorization` header, or a valid API key per FR-4.3.
- **FR-2.2**: `GET /api/links` (list links) requires a valid access token or API key, and returns only links owned by the authenticated user.
- **FR-2.3**: `GET /{linkId}` (visit/redirect) remains publicly accessible — no auth required.
- **FR-2.4**: Invalid or missing credentials return HTTP 401. Because FR-4.3 requires two credential types on the same routes, a plain API Gateway Cognito JWT authorizer is insufficient on its own — the design must resolve how both are verified.

### FR-3: Per-User Link Ownership

- **FR-3.1**: Every link record carries an `OwnerId` attribute holding the creating user's Cognito `sub`. The `sub` is used rather than email so that changing an email address never orphans links.
- **FR-3.1a**: `OwnerId` is **persistence-only and never appears on the wire**. It is added to the `ShortUrlRow` (persistence) struct and deliberately kept off `ShortUrl` (the serialized API shape), so the JSON response body for a link is byte-identical to today's. A caller already knows its own identity from the credential it presented, so returning the owner adds nothing but leaks an internal identifier. This also means the HTMX fragment templates need no ownership-related change.
- **FR-3.2**: `GET /api/links` returns only links whose `OwnerId` matches the caller. Ownership is derived from the verified credential server-side and is never accepted from the request body, a header, or a query parameter.
- **FR-3.3**: Per-user listing is served by an index keyed on owner and ordered by timestamp, so a user's links are retrieved and paginated without scanning other users' records. Existing pagination behaviour (page size, cursor semantics) is preserved.
- **FR-3.4**: Link IDs remain globally unique across all users — the short-link namespace is shared even though listings are private. Two users cannot be issued the same `LinkId`.
- **FR-3.5**: A link is listed only for its owner, but remains publicly resolvable by anyone holding the short URL (see FR-6). Ownership controls management, not redirect access.
- **FR-3.6**: A request presenting a valid credential for user A cannot read, or cause the creation of a link owned by, user B.

### FR-4: API Key Authentication (Machine-to-Machine)

API keys are krtk.rs-owned credentials stored in DynamoDB, managed through a dedicated `/api/keys` endpoint. They are not Cognito credentials — Cognito authenticates the *human* who mints and revokes them.

- **FR-4.1**: An authenticated user (Cognito JWT, browser session) can mint an API key via `POST /api/keys`. The plaintext key is returned exactly once in that response and never retrievable again.
- **FR-4.2**: An API key is presented on API requests via an `X-Api-Key` header. A request carrying a valid key is treated as the minting user — created links are owned by that user's Cognito `sub`, and listing returns only that user's links.
- **FR-4.3**: `POST /api/links` and `GET /api/links` accept **either** a Cognito JWT (`Authorization: Bearer <token>`) **or** an API key (`X-Api-Key`). Presenting neither returns HTTP 401.
- **FR-4.4**: An API key grants exactly the same scope as the owning user's browser session — create links, list own links. It grants nothing more: an API key cannot mint or revoke keys, so a leaked key cannot be used to provision its own replacements.
- **FR-4.5**: `GET /api/keys` lists the caller's keys as metadata only — key ID, label, creation time, last-used time, expiry. Never the secret.
- **FR-4.6**: `DELETE /api/keys/{keyId}` revokes a key immediately. A revoked key's next request returns HTTP 401.
- **FR-4.7**: A user may hold at most 10 active keys, each with a user-supplied label (e.g. "laptop CLI") so keys are distinguishable at revocation time.
- **FR-4.8**: Keys carry an optional expiry at mint time (default: no expiry, maximum: 365 days). An expired key is rejected like a revoked one.
- **FR-4.9**: Only the key's SHA-256 hash is persisted. Verification hashes the presented key and looks up by hash — the plaintext exists only in the mint response.

### FR-5: Frontend Authentication Flow

The frontend stays a static HTMX page. Its only auth responsibilities are: redirect out when unauthenticated, complete the code exchange on return, attach the access token to API calls, and refresh it when stale.

- **FR-5.1**: On load, the page checks for a usable session. With none, it redirects to the Cognito Hosted UI login URL. No link-shortening UI is shown to an unauthenticated visitor.
- **FR-5.2**: A dedicated callback page receives the authorization code, exchanges it for tokens using the stored PKCE verifier, and redirects to the main page. The code and PKCE verifier are single-use and discarded after exchange.
- **FR-5.3**: The access token is attached as `Authorization: Bearer <token>` on every `/api/*` request, including HTMX-issued requests.
- **FR-5.4**: When the access token is expired or within 5 minutes of expiry, the frontend silently exchanges the refresh token for a new one before issuing the request. A failed refresh (revoked or expired refresh token) redirects to login rather than surfacing an error.
- **FR-5.5**: A 401 from any `/api/*` call triggers one refresh attempt; if that also fails, the frontend clears its stored tokens and redirects to login.
- **FR-5.6**: The page shows the signed-in user's email and a sign-out control. Sign-out clears local tokens **and** calls Cognito's logout endpoint so the Hosted UI session is also ended — otherwise the next login silently re-authenticates with no password and no TOTP prompt, which looks like broken 2FA.
- **FR-5.7**: The page provides an API key management surface (mint with label + optional expiry, list, revoke) backed by `/api/keys`. A newly minted key is displayed once with an explicit warning that it will not be shown again.
- **FR-5.8**: Tokens are held in `sessionStorage`, scoped to the tab, and cleared on sign-out. They are not written to `localStorage` or to a non-`HttpOnly` cookie.
- **FR-5.9**: Every surface this spec adds or changes — the signed-in header, the sign-out control, the API key management area, the callback page, and any auth error or "redirecting" state — supports dark mode consistently with the rest of the site, per FR-9. New auth surfaces carry `dark:` variants rather than introducing light-only markup.
- **FR-5.10**: The theme preference persists in `localStorage` and is therefore **independent of the auth session**. Signing out clears tokens from `sessionStorage` (FR-5.8) but must not reset the user's chosen theme, and the FR-9.2 boot script must run on the callback page too so a returning user does not see a white flash mid-redirect.

### FR-6: Public Link Visits Unaffected

- **FR-6.1**: Short link redirects (`GET /{linkId}`) continue to work for all visitors without authentication.
- **FR-6.2**: Analytics (CloudFront realtime logs → Kinesis → `process_analytics` Lambda) remain unchanged.
- **FR-6.3**: The OAuth callback path must be carved out of the existing catch-all routing. CloudFront currently sends `/?*` to API Gateway and `/{linkId}` resolves to `visit_link`, so without an explicit behaviour the redirect back from Cognito would be interpreted as a visit to a short link named after the callback path.

### FR-7: Migration of Existing Links

The production table holds links created before authentication existed; they have no `OwnerId`. These are assigned to the admin's account so they remain visible and manageable.

- **FR-7.1**: A one-time migration assigns every pre-existing link an `OwnerId` equal to the admin's Cognito `sub`, so they appear in the admin's link list exactly as before.
- **FR-7.2**: The migration runs after the User Pool exists and the admin user has been created — the `sub` is not knowable before that point. Ordering is therefore: deploy pool → create admin user → capture `sub` → migrate → enable enforcement on the API. Enforcing before the backfill would leave the admin logged in and staring at an empty list, which reads as data loss.
- **FR-7.3**: The migration is idempotent: re-running it does not duplicate records, does not overwrite an `OwnerId` already set, and does not alter click counts, timestamps, or scraped metadata.
- **FR-7.4**: Every pre-existing short link continues to resolve correctly during and after migration. No redirect breaks at any point in the sequence.
- **FR-7.5**: The migration reports how many records it inspected, updated, and skipped, so completeness can be confirmed rather than assumed.
- **FR-7.6**: After migration, no link remains without an `OwnerId`. A link that somehow lacks one is treated as owned by nobody: still publicly resolvable, never listed for any user.
- **FR-7.7**: The migration is re-runnable at any later point to catch stragglers, with the same idempotency guarantees.

### FR-8: Auth Domain and Certificate

The login page is served from a krtk.rs-branded hostname so a user never types their password into an `amazoncognito.com` URL.

- **FR-8.1**: The Cognito Hosted UI is served at `auth.krtk.rs`. The default `*.auth.<region>.amazoncognito.com` domain is not used for user-facing sign-in.
- **FR-8.2**: A dedicated ACM certificate covering `auth.krtk.rs` is issued in `us-east-1`, which Cognito requires for a custom domain regardless of the User Pool's own region. The existing `CertificateStack` already runs in `us-east-1` and is the correct home for it.
- **FR-8.3**: The certificate is DNS-validated against the existing `krtk.rs` hosted zone — no manual validation step.
- **FR-8.4**: A Route53 alias record points `auth.krtk.rs` at the CloudFront distribution Cognito provisions for the custom domain.
- **FR-8.5**: The apex `krtk.rs` must keep a resolvable A record for Cognito to accept the custom domain. This is already satisfied by the existing CloudFront alias record, and must not be removed as part of this work.
- **FR-8.6**: Custom domain provisioning is slow — Cognito creates its own distribution and does not finish when CDK reports success — so it is treated as a distinct, explicitly verified step. A "deploy succeeded but login 404s" state is otherwise very confusing.
- **FR-8.7**: Switching to the default Cognito domain remains possible by changing the domain configuration and the frontend's authority URL — no application logic depends on which domain serves the Hosted UI.

### FR-9: Dark Mode (absorbed from PR #8)

PR #8 (`feature/dark-mode`) is **superseded by this spec rather than merged**: this spec's frontend work implements dark mode as part of the auth frontend, and PR #8 is closed afterwards with a reference to the commit that replaces it. The implementation below is transcribed from that PR so nothing is lost when the branch goes away — it is a reproduction target, not a fresh design.

Scope note: PR #8 touched 8 files but only `website/index.html` and `website/assets/main.js` collide with FR-5. The remaining work (three HTMX templates, two legal pages) does not conflict and must still be carried over, or closing #8 silently drops it.

- **FR-9.1**: Tailwind's Play CDN is configured for the `class` dark-mode strategy (`tailwind.config = { darkMode: 'class' }`), with `dark` toggled on the `<html>` element. This is required for a manual toggle rather than pure OS following.
- **FR-9.2**: An inline boot script runs **before** the Tailwind and HTMX script tags and before first paint. It reads `localStorage.getItem('theme')`, falls back to `window.matchMedia('(prefers-color-scheme: dark)')` when unset, and adds or removes the `dark` class accordingly. It is wrapped in `try`/`catch` and fails open to light theme, because `localStorage` can throw in restricted contexts. Preventing a flash of the wrong theme depends on this running early — it must not be moved into `main.js` or deferred.
- **FR-9.3**: A sun/moon toggle button (`id="theme-toggle"`) sits in the page header and is accessible: it carries `aria-pressed` reflecting the current state, kept in sync whenever the theme changes.
- **FR-9.4**: Toggle logic lives in `website/assets/main.js`: it flips the `dark` class on `<html>`, persists `'dark'` or `'light'` to `localStorage`, and re-reflects `aria-pressed`. Persistence failure is caught and ignored so the toggle still works for the session. Initialisation handles both the `loading` state (via `DOMContentLoaded`) and the already-parsed case.
- **FR-9.5**: `dark:` variants are applied across the server-rendered HTMX fragments — `shared/templates/links_table.html`, `new_short_link.html`, and `error_popup.html` — so swapped-in content matches the surrounding page. No Askama template logic, `hx-*` attribute, or `copyToClipboard` handler changes.
- **FR-9.6**: The `.spinner` CSS gets a `.dark .spinner` rule softening the light-gray base ring to `#4b5563` while keeping the `#008000` green brand accent on top, so the loader does not glare against a dark background.
- **FR-9.7**: The no-JS legal pages (`website/privacy`, `website/terms`) get a `@media (prefers-color-scheme: dark)` block. They follow the OS setting rather than the manual toggle, since they load no JavaScript. This is a known and accepted inconsistency: a manual toggle contradicting the OS preference will not be reflected on those two pages. They must not drop a dark-mode user onto a white page from the footer links.
- **FR-9.8**: The existing light-mode visual identity is preserved exactly — brand blue buttons, green spinner accent. Dark mode is additive.
- **FR-9.9**: PR #8's `.gitignore` change is **not** carried over as written. It replaces the `*.log` entry with `.agents/`, because it branched before the modernization merge that added `*.log` for `cdk deploy` transcripts. Both entries are wanted; the result must ignore `*.log` **and** `.agents/`.

## Non-Functional Requirements

### NFR-1: Security

- MFA (TOTP) is enforced at the User Pool level — users cannot opt out.
- Access and ID tokens live 1 hour; refresh tokens 30 days with rotation enabled.
- Tokens are held in `sessionStorage` only — never `localStorage`, never a readable cookie.
- The app client is public with PKCE; the implicit grant is disabled so no token ever appears in a URL fragment.
- API keys are stored as SHA-256 hashes and can be revoked immediately.
- CORS stops using `allowOrigins: ['*']` and is restricted to the krtk.rs origin, with `Authorization` and `X-Api-Key` added to the allowed headers.

### NFR-2: Scalability

- The per-owner index supports efficient pagination without full table scans.
- Cognito User Pool scales automatically — no infrastructure concern.

### NFR-3: Operational

- Existing links remain resolvable throughout the change — the redirect path is never gated, so link availability is independent of the auth rollout.
- The migration is a data backfill, not a table replacement: no export/import, no new table, no change to `LinkId` values, so no short URL changes.
- The rollout is ordered so the API is not enforcing auth until the admin user exists and can actually log in — avoiding a window where the site is locked out of itself.
- Deployment is zero-downtime; CloudFront and API Gateway absorb the cutover.

### NFR-4: Cost

- Cognito's free tier covers far more monthly active users than an invite-only deployment will ever have.
- No additional infrastructure cost beyond Cognito — API Gateway, Lambda, and DynamoDB are already in use.

## Acceptance Criteria

### Authentication
1. Unauthenticated requests to `POST /api/links` and `GET /api/links` return HTTP 401.
2. Loading krtk.rs while unauthenticated redirects to the Cognito Hosted UI; no shortening form is rendered first.
3. Completing the Hosted UI flow returns to krtk.rs authenticated, with the shortening form and the user's own links visible.
4. Login requires email/password AND a TOTP code; a first-time user is forced through TOTP enrolment before reaching the app.
5. The Hosted UI presents no sign-up option, and `sign_up` against the User Pool is rejected.
6. Sign-out clears local tokens and ends the Cognito session — returning to krtk.rs requires full re-authentication including TOTP.
7. An expired access token is refreshed transparently; a revoked refresh token redirects to login instead of erroring.
8. The login page loads over HTTPS at `auth.krtk.rs` with a valid certificate and no browser warning, and no user-facing step exposes an `amazoncognito.com` URL.

### Ownership
9. An authenticated user can create a short link and see it in their link list.
10. A second authenticated user cannot see links created by the first, and pagination for each user is unaffected by the other's link count.
11. Visiting a short link (`GET /{linkId}`) works without authentication and redirects correctly, including for a link owned by another user.
12. The JSON response body for a link is unchanged from before this feature — no `OwnerId`, no `owner_id`, no other new field. The only difference on `/api/links` is that the set of links returned is now scoped to the caller.

### API keys
13. A minted API key successfully creates and lists links with correct ownership; the plaintext key is shown once at mint and never again.
14. A revoked API key is rejected with HTTP 401 on its next request.
15. An API key cannot mint or revoke keys — `/api/keys` rejects `X-Api-Key` authentication.
16. An expired key is rejected exactly as a revoked one is.

### Migration
17. After migration, all pre-existing links appear in the admin's link list with their original click counts and timestamps intact.
18. Every pre-existing short URL still resolves after migration — no link changed value or broke.
19. Re-running the migration reports zero updates and changes nothing.

### Dark mode (absorbed from PR #8)
20. Toggling the theme persists across a full page reload, and no flash of the wrong theme is visible on load in either theme.
21. HTMX-swapped content (a newly shortened link, an appended table page, an error popup) matches the surrounding theme rather than arriving light-only.
22. The `/terms` and `/privacy` pages respect the OS dark preference and never present a white page to a dark-mode user arriving from the footer.
23. The toggle reports state to assistive tech via `aria-pressed`, and the spinner remains legible in dark mode.
24. `.gitignore` ignores both `*.log` and `.agents/`.

### Deployment
25. The OAuth callback path is not swallowed by the `/{linkId}` catch-all.
26. Every auth surface renders correctly in both themes, the callback page included, with no white flash during the redirect back from Cognito.
27. Signing out preserves the chosen theme — only tokens are cleared.
28. The system deploys cleanly via `cdk deploy --all` with no errors.
29. PR #8 is closed with a reference to the commit that supersedes it, and its branch is not merged.
