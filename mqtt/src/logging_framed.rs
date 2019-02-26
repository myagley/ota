#[derive(Debug)]
pub(crate) struct LoggingFramed<T>(tokio::codec::Framed<T, crate::proto::PacketCodec>)
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite;

impl<T> LoggingFramed<T>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite,
{
    pub(crate) fn new(io: T) -> Self {
        LoggingFramed(tokio::codec::Framed::new(io, Default::default()))
    }
}

impl<T> futures::Sink for LoggingFramed<T>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite,
{
    type SinkItem = <tokio::codec::Framed<T, crate::proto::PacketCodec> as futures::Sink>::SinkItem;
    type SinkError =
        <tokio::codec::Framed<T, crate::proto::PacketCodec> as futures::Sink>::SinkError;

    fn start_send(
        &mut self,
        item: Self::SinkItem,
    ) -> futures::StartSend<Self::SinkItem, Self::SinkError> {
        log::trace!(">>> {:?}", item);
        self.0.start_send(item)
    }

    fn poll_complete(&mut self) -> futures::Poll<(), Self::SinkError> {
        self.0.poll_complete()
    }
}

impl<T> futures::Stream for LoggingFramed<T>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite,
{
    type Item = <tokio::codec::Framed<T, crate::proto::PacketCodec> as futures::Stream>::Item;
    type Error = <tokio::codec::Framed<T, crate::proto::PacketCodec> as futures::Stream>::Error;

    fn poll(&mut self) -> futures::Poll<Option<Self::Item>, Self::Error> {
        let result = self.0.poll()?;
        if let futures::Async::Ready(Some(item)) = &result {
            log::trace!("<<< {:?}", item);
        }
        Ok(result)
    }
}
