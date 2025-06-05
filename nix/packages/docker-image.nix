{
  lib,
  locally-euclidean,
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
    ];

    config = {
      Env = [
        # These all should be set up, but you have to provide some actual values
        # "LOC_EUC_DB_CONNECTION_STRING=postgres://someuser:somepass@somehost:someport/somedb"
        # "OTEL_EXPORTER_OTLP_ENDPOINT=http://somehost:4317"
        # "OTEL_EXPORTER_OTLP_PROTOCOL=grpc"
        # "OTEL_TRACES_EXPORTER=otlp"
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
