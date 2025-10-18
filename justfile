# Extract values with fallbacks using `jq` and shell fallbacks
DISTRIBUTION_ID := `jq -r '.KrtkRsStack.distributionId // "NOT_DEPLOYED"' main-outputs.json 2>/dev/null || echo "NOT_DEPLOYED"`
GOOGLE_SECRET_ARN := `jq -r '.SecretsStack.googleApiSecretArn // "NOT_DEPLOYED"' secrets-outputs.json 2>/dev/null || echo "NOT_DEPLOYED"`

# Deploy and save outputs
deploy:
  @echo "⏳Deploying the CDK stack..."
  npx cdk deploy KrtkRsStack --outputs-file main-outputs.json

deploy-secrets-stack:
  @echo "⏳Deploying the Secrets CDK stack..."
  npx cdk deploy SecretsStack --outputs-file secrets-outputs.json

# Invalidate CDN Cache
invalidate-cache:
  @echo "⏳Clearing the CDN Cache..."
  aws cloudfront create-invalidation --distribution-id {{DISTRIBUTION_ID}} --paths "/*"

set-google-api-key:
    #!/usr/bin/env bash
    echo -n "Enter Google API key: "
    read -s api_key
    echo
    aws secretsmanager put-secret-value \
        --secret-id {{GOOGLE_SECRET_ARN}} \
        --secret-string "$api_key" \
        --profile personal \
        --region us-west-2
    echo "Secret updated successfully"

# Clean up
clean:
  rm -rf "*-outputs.json"

bye: clean
  cdk destroy --force
