{
  lib,
  locally-euclidean,
  cacert,
  tini,
  bash,
  coreutils,
  curl,
  dockerTools,
}:
let
  image = dockerTools.buildLayeredImage {
    name = "ghcr.io/mercurytechnologies/locally-euclidean";
    tag = "latest";
    maxLayers = 120;
    contents = [
      bash
      coreutils
      curl
      # Required for rustls-native-certs to have CA verification for e.g. opentelemetry
      cacert
    ];

    config = {
      Env = [
        # These all should be set up, but you have to provide some actual values
        # "LOC_EUC_DB_CONNECTION_STRING=postgres://someuser:somepass@somehost:someport/somedb"
        # "OTEL_EXPORTER_OTLP_ENDPOINT=http://somehost:4317"
        # "OTEL_EXPORTER_OTLP_PROTOCOL=grpc"
        # "OTEL_TRACES_EXPORTER=otlp"

        # I am uncertain whether this is necessary, since SURELY the port
        # redirects are done *from within* the container so listening on
        # [::1]:9000 should be fine, but I am committing this while trying to
        # fix a health check that makes no sense why it is failing and which is
        # not getting any requests through. Shrug!
        "LOC_EUC_BIND_ADDRESS=[::]:9000"
      ];
      Entrypoint = [
        "${lib.getExe tini}"
        "--"
      ];
      Cmd = [
        "${lib.getExe locally-euclidean}"
        "serve"
      ];
      ExposedPorts."9000/tcp" = { };
      Labels = {
        "org.opencontainers.image.title" = "locally-euclidean";
        "org.opencontainers.image.source" = "https://github.com/mercurytechnologies/locally-euclidean";
        "org.opencontainers.image.version" = locally-euclidean.version;
        "org.opencontainers.image.vendor" = "Mercury Technologies, Inc";
        "org.opencontainers.image.description" = "locally-euclidean deployment image";
      };
    };

  };
in
image.overrideAttrs (old: {
  # FIXME(jadel): stop doing overrideAttrs after we have https://github.com/NixOS/nixpkgs/pull/414030
  meta = {
    description = "Deployment OCI image for locally-euclidean";
    platforms = lib.platforms.linux;
  };
})
