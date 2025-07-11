{
  lib,
  writeShellApplication,
  docker-image,
  skopeo,
}:
writeShellApplication {
  name = "docker-push";
  runtimeInputs = [ skopeo ];
  text = ''
    set -xeu
    if [[ $# != 1 ]]; then
      echo "Usage: $0 REGISTRY_PATH" >&2
      exit 1
    fi
    registry_path=$1
    args=(
      # insecure-policy: we don't have any signature policy, we are just uploading an image.
      #
      # We *could* enable signing of docker images, but looking around I've not
      # seen any signed images elsewhere at the company after a brief look, and
      # the security model doesn't make sense to me: if you have creds to
      # upload an image, you surely have the creds to grab the signing key too,
      # at least in the way we'd implement it.
      --insecure-policy
      copy
      --all
      docker-archive:${docker-image}
      "docker://$registry_path"
    )
    skopeo "''${args[@]}"
  '';
  meta = {
    description = "Pushes the docker image to a registry";
    platforms = lib.platforms.linux;
  };
}
