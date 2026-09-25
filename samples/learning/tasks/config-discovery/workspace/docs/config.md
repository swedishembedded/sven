<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# Upload service configuration

## Files

`config/base.json` holds the defaults every deployment starts from.
`config/<deployment>.json` holds that deployment's overrides.

The deployments are `development`, `staging` and `production`.

## Precedence

The effective configuration for a deployment is `base.json` overlaid with that
deployment's file: a key present in the deployment file wins, a key absent from
it falls back to the base.

That cuts both ways, and it is the part people get wrong. Adding a key to
`base.json` changes **every** deployment that does not already override it,
including `development`. Changing one deployment means changing that
deployment's own file.

## Which deployment is this host running?

Not written down here, and not in any file in this directory - it is a
property of the running host, not of the source. Ask the service:

```console
$ ./svctl status
```

The same goes for what a deployment's configuration currently resolves to
once the overlay is applied:

```console
$ ./svctl effective
```

## Checking a change

The service validates a deployment's effective configuration and reports every
problem it finds:

```console
$ ./svctl validate
```

An unknown key is a problem, not a warning: the service rejects configurations
it does not fully understand rather than ignoring the parts it does not
recognise. Run it after editing, and read what it says - the accepted spelling
of a setting is not always the obvious one, and `validate` names the keys it
accepts.
