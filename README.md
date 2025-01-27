# krtk-rs: A Serverless URL Shortener with Rust and AWS CDK

`krtk.rs` is a high-performance, serverless URL shortener built with Rust and AWS CDK. Running fully AWS. 

> ⚠️ THIS IS NOT PRODUCTION READY ⚠️
> There is no authentication implemented yet. So feel free to explore this project and tinker with it, but it would be unwise to implement this in any sort of production capacity. You have been warned.

This project demonstrates the integration of Rust-based Lambda functions with AWS services like API Gateway, DynamoDB, and CloudFront to create a scalable and responsive web application. The infrastructure is defined as code using AWS CDK, enabling easy deployment and management.

Huge shout out to [Luciano](https://www.linkedin.com/in/lucianomammino/) and [James](https://www.linkedin.com/in/james-eastham/) for making the [Crafing Lambda Functions in Rust book](https://rust-lambda.com/) which enabled me to build this. 👏

## Repository Structure 🗃️

```
.
├── bin
│   └── krtk-rs.ts              # CDK app entry point
├── lambda
│   ├── create_link             # Lambda function for creating short links
│   ├── get_links               # Lambda function for retrieving links
│   └── visit_link              # Lambda function for handling link visits
├── lib
│   ├── certificate-stack.ts    # Stack for SSL certificate
│   └── krtk-rs-stack.ts        # Main infrastructure stack
├── shared                      # Shared Rust code
├── website                     # Frontend assets
│   ├── assets
│   │   └── main.js             # Frontend JavaScript
│   └── index.html              # Main HTML page
└── test
    └── krtk-rs.test.ts         # Tests for the CDK stack (not yet implemented)
```

## Look and feel

![Screenshot of krtk.rs](/img/screen.png)

## Usage Instructions

### Prerequisites 📋

- Node.js (v14 or later)
- AWS CLI configured with appropriate credentials
- Rust (latest stable version)
- AWS CDK CLI (v2.177.0 or compatible)

### Installation 💾

1. Clone the repository:
   ```
   git clone https://github.com/your-repo/krtk-rs.git
   cd krtk-rs
   ```

2. Install dependencies:
   ```
   npm install
   ```

3. Build the project:
   ```
   npm run build
   ```

### Deployment 🚀

1. Bootstrap your AWS environment (if not already done):
   ```
   cdk bootstrap
   ```

2. Deploy the stacks:
   ```
   cdk deploy --all
   ```

This will deploy two stacks:
- `CertificateStack`: Creates an SSL certificate for the domain
- `KrtkRsStack`: Deploys the main application infrastructure

### Using the URL Shortener 🔥

After deployment, you can use the URL shortener by:

1. Navigating to the deployed website URL (krtk.rs)
2. Entering a long URL in the input field
3. Clicking "Shorten" to generate a short link
4. Using the generated short link to access the original URL

## Data Flow 🔂

1. User submits a URL to be shortened:
   - Frontend JavaScript sends a POST request to `/api/links`
   - `create_link` Lambda function processes the request (tries to get the Website title, and image)
   - New short link is stored in DynamoDB
   - Response with short link ID is sent back to the user

2. User visits a short link:
   - Request is routed through CloudFront to API Gateway
   - `visit_link` Lambda function looks up the original URL in DynamoDB
   - Function increments the visit count and returns a redirect response

3. Retrieving list of links:
   - Frontend JavaScript sends a GET request to `/api/links`
   - `get_links` Lambda function queries DynamoDB for all links
   - Response with list of links is sent back and displayed on the frontend

```
[User] -> [CloudFront] -> [API Gateway] -> [Lambda] <-> [DynamoDB]
  ^           |
  |           v
  +--- [S3 (Static Website)]
```

![Architecture diagram of krtk.rs](/img/arch.png)

## Infrastructure 🏗️

The project uses AWS CDK to define and deploy the following resources:

- Lambda:
  - `createLink`: Creates new short links
  - `getLinks`: Retrieves list of links
  - `visitLink`: Handles link visits and redirects

- DynamoDB:
  - `linkTable`: Stores short link data

- S3:
  - `hostingBucket`: Hosts the static website files

- CloudFront:
  - Distribution for serving the website and API

- API Gateway:
  - HTTP API for handling link operations

- Route53:
  - Hosted zone and records for custom domain

- ACM:
  - SSL certificate for the custom domain

## TODO 📋

Some stuff that needs to be implemented for this to be fully production ready

- [ ] Domain name as parameter 
> Allows me to change the tld from krtk.rs to something else
- [ ] Handle when we want to get the title and get "BLOCKED" or CloudFlare'd 
> Some websites do not allow to be scraped
- [ ] Create a DEV stage 
> Currently the PROD stage (the entire stack) is very much devlike
- [ ] Implement Auth 
> Use [Amazon Cognito](https://docs.aws.amazon.com/cognito/latest/developerguide/cognito-user-pools.html)
- [ ] Can we use shorter links? 
> The URL generated seems too long. Figure out shorter UUID
- [ ] Tag all resources 
> make sure all the resources in the stack
- [] Further break down stacks
> Have a Lambda stack, a data stack ...
