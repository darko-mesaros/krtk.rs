/**
 * auth-config.js — Cognito configuration for the krtk.rs frontend.
 *
 * THIS FILE IS OVERWRITTEN AT DEPLOY TIME by CDK's Source.data() on the
 * BucketDeployment. The values below are obviously-fake placeholders so the
 * page can still load locally (auth will fail, but it won't throw on undefined).
 *
 * The CDK-generated version resolves the real pool ID, client ID, and auth
 * domain from CloudFormation tokens.
 */
window.KRTK_AUTH = {
  userPoolId: 'us-west-2_PLACEHOLDER',
  clientId: 'placeholder_client_id_0000000000',
  authDomain: 'auth.krtk.rs',
  region: 'us-west-2',
  redirectUri: 'https://krtk.rs/auth/callback',
  logoutUri: 'https://krtk.rs/',
};
