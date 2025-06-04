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
    # insecure-policy: we don't have any signature policy, we are just uploading an image
    skopeo --insecure-policy copy --all docker-archive:${docker-image} "docker://$registry_path"
  '';
  meta = {
    description = "Pushes the docker image to a registry";
    platforms = lib.platforms.linux;
  };
}
