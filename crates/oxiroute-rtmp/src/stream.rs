macro_rules! delegate_read_write {
    ($stream:ty { $($variant:ident),+ $(,)? }) => {
        impl std::io::Read for $stream {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                match self {
                    $(Self::$variant(stream) => std::io::Read::read(stream, buffer),)+
                }
            }
        }

        impl std::io::Write for $stream {
            fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
                match self {
                    $(Self::$variant(stream) => std::io::Write::write(stream, buffer),)+
                }
            }

            fn flush(&mut self) -> std::io::Result<()> {
                match self {
                    $(Self::$variant(stream) => std::io::Write::flush(stream),)+
                }
            }
        }
    };
}
