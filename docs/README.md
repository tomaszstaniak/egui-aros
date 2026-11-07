# egui-aros documentation

- [Development](development.md): toolchain, SDK, building the library and the examples, host tests, running on AROS.
- [Patching](patching.md): the pinned sources and how changes to them are kept.
- [Using egui-aros in a program](using.md): the `run` loop, the `App` trait, services, and what the library does and does not do.

Scope: built and run through the Craft ports on x86_64 ABIv11 (AROS One 1.3), x86_64 ABIv1 (mainline nightlies) and the Raspberry Pi (aarch64 ABIv1), all under QEMU. Real hardware has not been tried.
