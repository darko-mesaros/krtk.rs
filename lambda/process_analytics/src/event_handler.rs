use lambda_runtime::{tracing, Error, LambdaEvent};
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

pub(crate)async fn function_handler(event: LambdaEvent<KinesisEvent>) -> Result<(), Error> {
    // Extract some useful information from the request
    let records = event.payload.records;

    for record in records {
        let b64_data = &record.kinesis.data;
        let decoded_data: &[u8] = b64_data.as_ref();

        // Convert into string
        let string_data = String::from_utf8(decoded_data.to_vec())
            .expect("Failed to convert the Kinesis data from UTF-8 into a String");

        // Put it in a Struct
        let fields: Vec<&str> = string_data.split('\t').collect();
        let analytics = CfAnalyticsData {
            timestamp: fields[0].to_string(),
            source_ip: fields[1].to_string(),
            status_code: fields[2].to_string(),
            link_id: fields[3].to_string(),
        };
        tracing::info!("RECORD: {:?}", analytics);
    }
    

    Ok(())
}
