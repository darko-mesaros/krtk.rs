# Extract values with fallbacks using `jq` and shell fallbacks
DISTRIBUTION_ID := `jq -r '.KrtkRsStack.distributionId // "NOT_DEPLOYED"' outputs.json 2>/dev/null || echo "NOT_DEPLOYED"`

# Deploy and save outputs
deploy:
  @echo "⏳Deploying the CDK stack..."
  npx cdk deploy KrtkRsStack --outputs-file outputs.json

# Invalidate CDN Cache
invalidate-cache:
  @echo "⏳Clearing the CDN Cache..."
  aws cloudfront create-invalidation --distribution-id {{DISTRIBUTION_ID}} --paths "/*"

# Clean up
clean:
  rm -rf "outputs.json"

bye: clean
  cdk destroy --force
