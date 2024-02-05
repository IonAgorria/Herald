#[macro_export]
macro_rules! implement_asyncio_func {
    ($func:ident, match $enum_ty:ident { $($matcher:pat => $result:expr),* }) => {
        fn $func(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            {
                use $enum_ty::*;
                match self.project() {
                    $($matcher => $result),*
                }
            }.$func(cx)
        }
    };
    ($func:ident, $arg:ty, $ret:ty, match $enum_ty:ident { $($matcher:pat => $result:expr),* }) => {
        fn $func(self: Pin<&mut Self>, cx: &mut Context<'_>, arg: $arg) -> Poll<std::io::Result<$ret>> { 
            {
                use $enum_ty::*;
                match self.project() {
                    $($matcher => $result),*
                }
            }.$func(cx, arg)
        }
    };
}

#[macro_export]
macro_rules! implement_async_read_proxy {
    ($impl_type:ident, $impl_type_proj:ident, match { $($matcher:pat => $result:expr),* }) => {
        impl tokio::io::AsyncRead for $impl_type {
            $crate::implement_asyncio_func!(
                poll_read, &mut tokio::io::ReadBuf<'_>, (),
                match $impl_type_proj {
                    $($matcher => $result),*
                }
            );
        }
    };
}

#[macro_export]
macro_rules! implement_async_write_proxy {
    ($impl_type:ident, $impl_type_proj:ident, match { $($matcher:pat => $result:expr),* }) => {
        impl tokio::io::AsyncWrite for $impl_type {
            $crate::implement_asyncio_func!(
                poll_write, &[u8], usize,
                match $impl_type_proj {
                    $($matcher => $result),*
                }
            );
            $crate::implement_asyncio_func!(
                poll_flush,
                match $impl_type_proj {
                    $($matcher => $result),*
                }
            );
            $crate::implement_asyncio_func!(
                poll_shutdown,
                match $impl_type_proj {
                    $($matcher => $result),*
                }
            );

            $crate::implement_asyncio_func!(
                poll_write_vectored, &[std::io::IoSlice<'_>], usize,
                match $impl_type_proj {
                    $($matcher => $result),*
                }
            );

            fn is_write_vectored(&self) -> bool {
                use $impl_type::*;
                match self {
                    $($matcher => $result),*
                }.is_write_vectored()
            }
        }
    };
}