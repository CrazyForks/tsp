#![no_main]

use libfuzzer_sys::fuzz_target;
use tsp_sdk::cesr;

fuzz_target!(|data: cesr::fuzzing::Wrapper| {
    let mut buf = Vec::new();
    match cesr::encode_payload(&data.0, None, &mut buf) {
        Ok(()) => {
            let result: cesr::DecodedPayload = cesr::decode_payload(&mut buf).unwrap();

            assert_eq!(data, result.payload);
        }
        // MissingHops is only raised for RoutedMessage with an empty hop list
        Err(cesr::error::EncodeError::MissingHops) => {
            assert!(matches!(
                &data.0,
                cesr::Payload::RoutedMessage(route, _) if route.is_empty()
            ));
        }
        // Fields that exceed the CESR variable-data size limit are legitimately rejected
        Err(cesr::error::EncodeError::ExcessiveFieldSize) => {}
        // Parallel-relation payloads with an empty new_vid are legitimately rejected
        Err(cesr::error::EncodeError::InvalidVid) => {}
        // Any other error is not expected from encode_payload — surface it as a finding
        Err(e) => panic!("unexpected encode error: {e:?}"),
    }
});
