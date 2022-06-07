use std::io;
use std::io::{Read, Write};

#[derive(Debug, PartialEq, Clone)]
pub enum Command {
    Name(String),
    Message(String),
}

pub trait Serialize {
    fn encode(&mut self, cmd: Command) -> io::Result<()>;
}

pub trait Deserialize {
    fn decode(&mut self) -> io::Result<Command>;
}

impl<S> Serialize for S
where
    S: Write,
{
    fn encode(&mut self, cmd: Command) -> io::Result<()> {
        let bytes = match cmd {
            Command::Name(name) => {
                let name_bytes = name.as_bytes();
                let name_bytes_len = name_bytes.len();
                let mut bytes = Vec::with_capacity(name_bytes_len + 5);
                // tag of name
                bytes.push(0);
                bytes.extend((name_bytes_len as u32).to_be_bytes());
                bytes.extend(name_bytes);

                bytes
            }
            Command::Message(msg) => {
                let msg_bytes = msg.as_bytes();
                let msg_bytes_len = msg_bytes.len();
                let mut bytes = Vec::with_capacity(msg_bytes_len + 5);
                // tag of message
                bytes.push(1);
                bytes.extend((msg_bytes_len as u32).to_be_bytes());
                bytes.extend(msg_bytes);

                bytes
            }
        };

        self.write_all(&bytes)?;
        self.flush()?;

        Ok(())
    }
}

impl<S> Deserialize for S
where
    S: Read,
{
    fn decode(&mut self) -> io::Result<Command> {
        let mut tag_byte = [0u8];
        let mut len_bytes = [0u8; 4];

        self.read_exact(&mut tag_byte)?;
        self.read_exact(&mut len_bytes)?;

        let tag = tag_byte[0];
        let len = u32::from_be_bytes(len_bytes) as usize;

        let mut msg_bytes = vec![0u8; len];
        self.read_exact(&mut msg_bytes)?;
        let msg = String::from_utf8(msg_bytes).unwrap();

        match tag {
            // name
            0 => Ok(Command::Name(msg)),
            // message
            1 => Ok(Command::Message(msg)),
            tag => unimplemented!("unknown message tag {}", tag),
        }
    }
}

pub fn would_block(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::WouldBlock
}

// To make the methods in Serialize and Deserialize more easily to reuse in event-loop server, the StreamAgent
// wraps the underlying stream and implemented Write and Read trait. Its buffer could be optimized, this example
// only uses the simplest Vec<u8> as it.
pub mod stream_agent {
    use crate::would_block;
    use std::io::{Read, Write};
    use std::{cmp, io};

    pub struct StreamAgent<S> {
        stream: S,
        reader: AgentReader,
        writer: AgentWriter,
    }

    impl<S> StreamAgent<S>
    where
        S: Read + Write,
    {
        pub fn new(stream: S) -> Self {
            Self {
                stream,
                reader: AgentReader::default(),
                writer: AgentWriter::default(),
            }
        }

        pub fn read_from_stream(&mut self) -> io::Result<()> {
            match self.stream.read_to_end(&mut self.reader.buf) {
                Ok(0) => Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(_) => Ok(()),
                Err(err) => {
                    if would_block(&err) {
                        Ok(())
                    } else {
                        Err(err)
                    }
                }
            }
        }

        pub fn principal_ref_mut(&mut self) -> &mut S {
            &mut self.stream
        }
    }

    impl<S> Read for StreamAgent<S>
    where
        S: Read,
    {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let len_min = cmp::min(self.reader.buf.len(), buf.len());
            let slice = self.reader.buf.drain(0..len_min);
            buf[0..len_min].copy_from_slice(slice.as_slice());
            Ok(slice.len())
        }
    }

    impl<S> Write for StreamAgent<S>
    where
        S: Write,
    {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.writer.buf.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            let mut buf = &self.writer.buf[self.writer.cursor..];

            while !buf.is_empty() {
                match self.stream.write(buf) {
                    Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                    Ok(n) => {
                        self.writer.cursor += n;
                        buf = &buf[self.writer.cursor..];
                    }
                    Err(err) => {
                        return if would_block(&err) { Ok(()) } else { Err(err) };
                    }
                };
            }

            self.writer.clear();
            Ok(())
        }
    }

    #[derive(Default)]
    pub struct AgentWriter {
        buf: Vec<u8>,
        // this is needed, because it will return when write operation returns `WouldBlock` error.
        cursor: usize,
    }

    impl AgentWriter {
        pub fn clear(&mut self) {
            self.cursor = 0;
            self.buf.clear()
        }
    }

    #[derive(Default)]
    pub struct AgentReader {
        buf: Vec<u8>,
    }
}

// Serialize and Deserialize trait don't work in async world. The AsyncStreamAgent implemented Stream
// and Sink trait to make the user more convenient to read Command structs from or write them to
// the wrapped underlying stream.
pub mod async_stream_agent {
    use crate::{Command, Serialize};
    use futures::prelude::*;
    use futures::{io, ready, AsyncRead, AsyncReadExt, AsyncWrite, Stream};
    use std::pin::Pin;
    use std::task::{Context, Poll};

    pub struct AsyncStreamAgent<S> {
        stream: S,
        writer: AgentAsyncWriter,
    }

    impl<S> AsyncStreamAgent<S>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        pub fn new(stream: S) -> Self {
            Self {
                stream,
                writer: AgentAsyncWriter::default(),
            }
        }
    }

    impl<S> Unpin for AsyncStreamAgent<S> where S: Unpin {}

    impl<S> Stream for AsyncStreamAgent<S>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        type Item = io::Result<Command>;

        fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            let fut = read_frame(&mut self.stream);
            Box::pin(fut).poll_unpin(cx).map(Some)
        }
    }

    impl<S> Sink<Command> for AsyncStreamAgent<S>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        type Error = io::Error;

        fn poll_ready(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn start_send(self: Pin<&mut Self>, item: Command) -> Result<(), Self::Error> {
            let this = self.get_mut();
            this.writer.buf.encode(item)?;

            Ok(())
        }

        fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            let this = self.get_mut();

            let mut buf = &this.writer.buf[this.writer.cursor..];

            while !buf.is_empty() {
                let n = ready!(Pin::new(&mut this.stream).poll_write(cx, buf))?;
                this.writer.cursor += n;
                buf = &buf[this.writer.cursor..];
            }

            this.writer.clear();

            ready!(Pin::new(&mut this.stream).poll_flush(cx)?);
            Poll::Ready(Ok(()))
        }

        fn poll_close(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            ready!(self.as_mut().poll_flush(cx))?;

            ready!(Pin::new(&mut self.stream).poll_close(cx))?;
            Poll::Ready(Ok(()))
        }
    }

    async fn read_frame<S>(stream: &mut S) -> io::Result<Command>
    where
        S: AsyncRead + Unpin,
    {
        let mut tag_byte = [0u8];
        let mut len_bytes = [0u8; 4];

        stream.read_exact(&mut tag_byte).await?;
        stream.read_exact(&mut len_bytes).await?;

        let tag = tag_byte[0];
        let len = u32::from_be_bytes(len_bytes) as usize;

        let mut msg_bytes = vec![0u8; len];
        stream.read_exact(&mut msg_bytes).await?;
        let msg = String::from_utf8(msg_bytes).unwrap();

        match tag {
            // name
            0 => Ok(Command::Name(msg)),
            // message
            1 => Ok(Command::Message(msg)),
            tag => unimplemented!("unknown message tag {}", tag),
        }
    }

    #[derive(Default)]
    pub struct AgentAsyncWriter {
        buf: Vec<u8>,
        cursor: usize,
    }

    impl AgentAsyncWriter {
        pub fn clear(&mut self) {
            self.cursor = 0;
            self.buf.clear()
        }
    }
}

#[cfg(test)]
pub mod utils {
    use futures::io::{AsyncRead, AsyncWrite};
    use std::io::{Read, Write};
    use std::pin::Pin;
    use std::task::{Context, Poll};

    pub struct DummyStream {
        pub buf: Vec<u8>,
    }

    impl Read for DummyStream {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let len_min = std::cmp::min(self.buf.len(), buf.len());
            let slice = self.buf.drain(0..len_min);
            buf[0..len_min].copy_from_slice(slice.as_slice());
            Ok(slice.len())
        }
    }

    impl Write for DummyStream {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.buf.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl AsyncRead for DummyStream {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut [u8],
        ) -> Poll<std::io::Result<usize>> {
            let len_min = std::cmp::min(self.buf.len(), buf.len());
            let slice = self.get_mut().buf.drain(0..len_min);
            buf[0..len_min].copy_from_slice(slice.as_slice());
            Poll::Ready(Ok(slice.len()))
        }
    }

    impl AsyncWrite for DummyStream {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            self.get_mut().buf.extend_from_slice(buf);
            Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::async_stream_agent::AsyncStreamAgent;
    use crate::utils::DummyStream;
    use async_std::task;
    use futures::prelude::*;

    #[test]
    fn encode_decode_should_work() {
        let name_cmd = Command::Name("Alice".into());
        let msg_cmd = Command::Message("Hello".into());

        let buf = Vec::new();
        let mut stream = DummyStream { buf };
        stream.encode(name_cmd.clone()).unwrap();
        stream.encode(msg_cmd.clone()).unwrap();

        assert_eq!(stream.decode().unwrap(), name_cmd);
        assert_eq!(stream.decode().unwrap(), msg_cmd);
    }

    #[test]
    fn async_stream_agent_should_work() -> io::Result<()> {
        task::block_on(async {
            let buf = Vec::new();
            let stream = DummyStream { buf };
            let mut stream = AsyncStreamAgent::new(stream);
            let cmd = Command::Message("Hello world".into());

            stream.send(cmd.clone()).await?;

            if let Some(Ok(c)) = stream.next().await {
                assert_eq!(c, cmd);
            } else {
                assert!(false)
            }

            Ok(())
        })
    }
}
