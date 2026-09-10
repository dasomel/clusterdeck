# ClusterDeck Adoption Guide

> First success is an end-to-end workstation access flow: **Profile -> SSH -> kubeconfig -> Kubernetes API**.

## 1. Evaluate the implemented core before the roadmap

ClusterDeck is an early-stage desktop/workstation tool. Treat README/design roadmap items separately from features proven in the current build. Do not infer packaged-app or platform support from design intent alone.

## 2. First verified success

Use a non-production test target and verify this sequence:

1. Create or select a connection profile.
2. Establish the SSH boundary using the documented authentication method.
3. Retrieve or select the intended kubeconfig/context.
4. Connect to the Kubernetes API.
5. Read a harmless resource such as cluster/node metadata.
6. Disconnect and reconnect to prove the profile is reusable.

Record which step failed; "connection failed" is too coarse for troubleshooting.

## 3. Security boundary

SSH credentials, kubeconfig material, local key storage, host verification, and any command execution are security-sensitive. New convenience features must not silently widen filesystem, process, credential, or Kubernetes permissions.

## 4. Documentation path

- README — product intent and current setup
- architecture/MVP documents — design context
- this guide — external first-success acceptance path

## 5. Documentation rule

Mark screenshots, packaged-app instructions, supported macOS targets, and planned workflows as implemented only when the corresponding source/build/release evidence exists.