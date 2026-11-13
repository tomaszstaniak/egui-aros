# Private libc adjustments

`0001-open-errno-on-mainline.patch`: on mainline x86_64 a failed `open()` left `errno` unset
(mainline posixc, since 2026-11-06); `rs_fs_open` works the reason out. Remove when the mainline
SDK in use sets it.

The two adjustments the LightCraft port carried here (memchr declaration, serialized filesystem
entry points of the v11 C library) are part of rust-aros since `b060307`.
