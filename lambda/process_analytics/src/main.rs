use lambda_runtime::{run, service_fn, tracing, Error, LambdaEvent};

use aws_lambda_events::event::kinesis::KinesisEvent;

//TODO: for next stream
// Do something with the struct
// Remove the `\n` from the `link_id` field
// Store it in DynamoDB
//  - Get update the clicks

#[derive(Debug)]
pub struct CfAnalyticsData {
    timestamp: String, // Should be f64 or u64
    source_ip: String,
    status_code: String, // Should be an ENUM?
    link_id: String,
}

pub async fn function_handler(event: LambdaEvent<KinesisEvent>) -> Result<(), Error> {
    // Extract some useful information from the request
    let records = event.payload.records;

    for record in records {
        let decoded_data: &[u8] = &record.kinesis.data;

        // Convert into string or fail if it does not work, something must be off
        let string_data = String::from_utf8(decoded_data.to_vec())
            .expect("Failed to convert the Kinesis data from UTF-8 into a String");

        // Data coming in looks like this:
        // "1739035776.180\t24.18.218.96\t302\t/k120oizrul/n"
        // I know ... TSV 🙄
        let fields: Vec<&str> = string_data.split('\t').collect();

        // Put it in a Struct
        let analytics = CfAnalyticsData {
            timestamp: fields[0].to_string(),
            source_ip: fields[1].to_string(),
            status_code: fields[2].to_string(),
            link_id: fields[3]
                .trim()                     // .trim() removes the `/n`
                .trim_start_matches("/")    // Remove the "/" at the front
                .to_string(), 
        };
        tracing::info!("RECORD: {:?}", analytics);

        // TODO: Implement the actual functionality
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing::init_default_subscriber();

    run(service_fn(function_handler)).await
}
