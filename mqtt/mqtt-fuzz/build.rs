/// Writes the inputs for the fuzzer

use std::io::Write;

use tokio::codec::Encoder;

fn main() -> Result<(), Box<dyn std::error::Error>> {
	let in_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("in");
	std::fs::remove_dir_all(&in_dir)?;
	std::fs::create_dir(&in_dir)?;

	let packets = vec![
		("connack", mqtt::proto::Packet::ConnAck {
			session_present: true,
			return_code: mqtt::proto::ConnectReturnCode::Accepted,
		}),

		("connect", mqtt::proto::Packet::Connect {
			username: Some("username".to_string()),
			password: Some("password".to_string()),
			will: Some(mqtt::proto::Publication {
				topic_name: "will-topic".to_string(),
				qos: mqtt::proto::QoS::ExactlyOnce,
				retain: true,
				payload: b"\x00\x01\x02\xFF\xFE\xFD".to_vec(),
			}),
			client_id: mqtt::proto::ClientId::IdWithExistingSession("id".to_string()),
			keep_alive: std::time::Duration::from_secs(5),
		}),

		("disconnect", mqtt::proto::Packet::Disconnect),

		("pingreq", mqtt::proto::Packet::PingReq),

		("pingresp", mqtt::proto::Packet::PingResp),

		("puback", mqtt::proto::Packet::PubAck {
			packet_identifier: mqtt::proto::PacketIdentifier::new(5).unwrap(),
		}),

		("pubcomp", mqtt::proto::Packet::PubComp {
			packet_identifier: mqtt::proto::PacketIdentifier::new(5).unwrap(),
		}),

		("publish", mqtt::proto::Packet::Publish {
			packet_identifier_dup_qos: mqtt::proto::PacketIdentifierDupQoS::ExactlyOnce(mqtt::proto::PacketIdentifier::new(5).unwrap(), true),
			retain: true,
			topic_name: "publish-topic".to_string(),
			payload: b"\x00\x01\x02\xFF\xFE\xFD".to_vec(),
		}),

		("pubrec", mqtt::proto::Packet::PubRec {
			packet_identifier: mqtt::proto::PacketIdentifier::new(5).unwrap(),
		}),

		("pubrel", mqtt::proto::Packet::PubRel {
			packet_identifier: mqtt::proto::PacketIdentifier::new(5).unwrap(),
		}),

		("suback", mqtt::proto::Packet::SubAck {
			packet_identifier: mqtt::proto::PacketIdentifier::new(5).unwrap(),
			qos: vec![
				mqtt::proto::SubAckQos::Success(mqtt::proto::QoS::ExactlyOnce),
				mqtt::proto::SubAckQos::Failure,
			],
		}),

		("subscribe", mqtt::proto::Packet::Subscribe {
			packet_identifier: mqtt::proto::PacketIdentifier::new(5).unwrap(),
			subscribe_to: vec![
				mqtt::proto::SubscribeTo {
					topic_filter: "subscribe-topic".to_string(),
					qos: mqtt::proto::QoS::ExactlyOnce,
				},
			],
		}),

		("unsuback", mqtt::proto::Packet::UnsubAck {
			packet_identifier: mqtt::proto::PacketIdentifier::new(5).unwrap(),
		}),

		("unsubscribe", mqtt::proto::Packet::Unsubscribe {
			packet_identifier: mqtt::proto::PacketIdentifier::new(5).unwrap(),
			unsubscribe_from: vec![
				"unsubscribe-topic".to_string(),
			],
		}),
	];

	for (filename, packet) in packets {
		let file = std::fs::OpenOptions::new().create(true).write(true).open(in_dir.join(filename))?;
		let mut file = std::io::BufWriter::new(file);

		let mut codec: mqtt::proto::PacketCodec = Default::default();

		let mut bytes = bytes::BytesMut::new();

		codec.encode(packet, &mut bytes)?;

		file.write_all(&bytes)?;

		file.flush()?;
	}

	Ok(())
}
