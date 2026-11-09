use super::*;

impl AgentDevice {
    /// Bind replay to the same Matrix device. A replacement device cannot
    /// borrow another device's transaction IDs to retry an uncertain send.
    pub(in crate::console) fn file_device_id(&self) -> Result<String, &'static str> {
        if self.is_revoked() {
            return Err("file_device_unavailable");
        }
        Ok(format!(
            "{}|{}|{}",
            self.client.homeserver(),
            self.client.user_id().ok_or("file_device_unavailable")?,
            self.client.device_id().ok_or("file_device_unavailable")?
        ))
    }

    pub(in crate::console) async fn upload_room_file(
        &self,
        bytes: Vec<u8>,
    ) -> Result<String, &'static str> {
        self.file_device_id()?;
        if bytes.len() > crate::documents::MAX_FILE_BYTES {
            return Err("file_size_limit");
        }
        let config = matrix_sdk::config::RequestConfig::new()
            .timeout(Duration::from_secs(30))
            .retry_limit(0);
        let mime = "application/octet-stream"
            .parse()
            .map_err(|_| "invalid_attachment")?;
        self.client
            .media()
            .upload(&mime, bytes, Some(config))
            .await
            .map(|r| r.content_uri.to_string())
            .map_err(|_| "file_upload_outcome_unknown")
    }

    pub(in crate::console) async fn publish_room_file(
        &self,
        room: &str,
        transaction: &str,
        content: serde_json::Value,
    ) -> Result<String, &'static str> {
        self.publish_room_file_with_security(room, transaction, content, true).await
    }
    pub(in crate::console) async fn publish_room_file_with_security(
        &self, room: &str, transaction: &str, content: serde_json::Value, encrypted: bool,
    ) -> Result<String, &'static str> {
        self.file_device_id()?;
        let room_id = OwnedRoomId::try_from(room).map_err(|_| "file_room_unavailable")?;
        let joined = self
            .client
            .get_room(&room_id)
            .ok_or("file_room_unavailable")?;
        if joined.state() != matrix_sdk::RoomState::Joined
            || joined
                .latest_encryption_state()
                .await
                .map_err(|_| "file_room_unavailable")?
                .is_encrypted() != encrypted
        {
            return Err("file_room_unavailable");
        }
        // The encrypted media descriptor is itself sent in an encrypted room
        // event. Never fall back to a plaintext room after state changes.
        self.send_room_message(room, transaction, content)
            .await
            .map_err(|_| "file_send_outcome_unknown")
    }
}
