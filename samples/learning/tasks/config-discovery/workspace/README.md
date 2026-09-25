<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# upload-service

Uploads artefacts to the artefact store. One service, several deployments,
one configuration directory.

- `config/` - see [docs/config.md](docs/config.md) for how the files overlay.
- `svctl` - inspect and validate the service running on this host.
- `src/uploader.py` - the client the service runs.

The service is already running on this host. `./svctl --help` lists what you
can ask it.
