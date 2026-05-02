fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Allow Gradle/Android to bake a custom version number
    println!("cargo:rerun-if-env-changed=PENUMBRA_VERSION");
    let version =
        std::env::var("PENUMBRA_VERSION").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string());
    println!("cargo:rustc-env=PENUMBRA_VERSION={version}");

    tonic_prost_build::configure()
        .build_server(true)
        .build_client(false)
        .compile_protos(
            &[
                "proto/humane/aibus/aibus.proto",
                "proto/humane/pushrelay/pushrelay.proto",
                "proto/humane/featureflags/featureflags.proto",
                "proto/humane/account/account.proto",
                "proto/humane/contacts/contacts.proto",
                "proto/humane/events/events.proto",
                "proto/humane/provisioning/provisioning.proto",
                "proto/humane/capture/capture.proto",
                "proto/humane/common/encryption.proto",
                "proto/humane/partnerservices/partnerservices.proto",
                "proto/humane/privacy/privacy.proto",
                "proto/humane/privacy/privacy_common.proto",
            ],
            &["proto"],
        )?;
    Ok(())
}
