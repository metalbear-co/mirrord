tonic::include_proto!("mirrord.api");

impl BincodeMessage {
    pub fn as_bincode<D>(&self) -> Result<D, bincode::error::DecodeError>
    where
        D: bincode::Decode,
    {
        bincode::decode_from_slice(&self.encoded, bincode::config::standard()).map(|(buf, _)| buf)
    }

    pub fn from_bincode<E>(value: E) -> Result<Self, bincode::error::EncodeError>
    where
        E: bincode::Encode,
    {
        bincode::encode_to_vec(value, bincode::config::standard())
            .map(|encoded| BincodeMessage { encoded })
    }
}
