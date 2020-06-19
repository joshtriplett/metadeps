`system-deps` lets you write system dependencies in `Cargo.toml` metadata,
rather than programmatically in `build.rs`. This makes those dependencies
declarative, so other tools can read them as well.

For now only `pkg-config` dependencies are supported but we are planning to
[expand it](https://github.com/gdesmott/system-deps/issues/3) at some point.

`system-deps` has been started as a fork of the
[metadeps](https://github.com/joshtriplett/metadeps) project.

# Usage

In your `Cargo.toml`, add the following to your `[build-dependencies]`:

```toml
system-deps = "1.1"
```

Then, to declare a dependency on `testlib >= 1.2`, a conditional dependency
on `testdata >= 4.5` and a dependency on `glib-2.0 >= 2.64`
add the following section:

```toml
[package.metadata.system-deps]
testlib = "1.2"
testdata = { version = "4.5", feature = "use-testdata" }
glib = { name = "glib-2.0", version = "2.64" }
```

In your `build.rs`, add:

```rust
fn main() {
    system_deps::Config::new().probe().unwrap();
}
```

Dependency versions can also be controlled using features:

```toml
[features]
v1_2 = []
v1_4 = ["v1_4"]

[package.metadata.system-deps]
gstreamer = { name = "gstreamer-1.0", version = "1.0", feature-versions = { v1_2 = "1.2", v1_4 = "1.4" }}
```

In this case the highest version among enabled features will be used.
