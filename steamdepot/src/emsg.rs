#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum EMsg {
    Multi = 1,

    ClientHeartBeat = 703,

    ClientLogon = 5514,
    ClientLogOnResponse = 751,
    ClientLogOff = 706,

    ChannelEncryptRequest = 1303,
    ChannelEncryptResponse = 1304,
    ChannelEncryptResult = 1305,

    ClientLicenseList = 780,
    ClientRequestFreeLicense = 5572,
    ClientRequestFreeLicenseResponse = 5573,

    ClientPICSAccessTokenRequest = 8905,
    ClientPICSAccessTokenResponse = 8906,
    ClientPICSProductInfoRequest = 8903,
    ClientPICSProductInfoResponse = 8904,

    ClientGetDepotDecryptionKey = 5438,
    ClientGetDepotDecryptionKeyResponse = 5439,
    ClientGetAppOwnershipTicket = 857,
    ClientGetAppOwnershipTicketResponse = 858,

    ClientSessionToken = 850,

    ServiceMethodSendToClient = 147,
    ServiceMethodCallFromClient = 151,

    ServiceMethodCallFromClientNonAuthed = 9804,
    ServiceMethodResponse = 9805,
}

impl EMsg {
    pub fn from_u32(value: u32) -> Option<Self> {
        match value {
            1 => Some(Self::Multi),

            703 => Some(Self::ClientHeartBeat),
            5514 => Some(Self::ClientLogon),
            751 => Some(Self::ClientLogOnResponse),
            706 => Some(Self::ClientLogOff),

            1303 => Some(Self::ChannelEncryptRequest),
            1304 => Some(Self::ChannelEncryptResponse),
            1305 => Some(Self::ChannelEncryptResult),

            780 => Some(Self::ClientLicenseList),
            5572 => Some(Self::ClientRequestFreeLicense),
            5573 => Some(Self::ClientRequestFreeLicenseResponse),

            8905 => Some(Self::ClientPICSAccessTokenRequest),
            8906 => Some(Self::ClientPICSAccessTokenResponse),
            8903 => Some(Self::ClientPICSProductInfoRequest),
            8904 => Some(Self::ClientPICSProductInfoResponse),

            5438 => Some(Self::ClientGetDepotDecryptionKey),
            5439 => Some(Self::ClientGetDepotDecryptionKeyResponse),
            857 => Some(Self::ClientGetAppOwnershipTicket),
            858 => Some(Self::ClientGetAppOwnershipTicketResponse),

            850 => Some(Self::ClientSessionToken),

            147 => Some(Self::ServiceMethodSendToClient),
            151 => Some(Self::ServiceMethodCallFromClient),

            9804 => Some(Self::ServiceMethodCallFromClientNonAuthed),
            9805 => Some(Self::ServiceMethodResponse),

            _ => None,
        }
    }
}
