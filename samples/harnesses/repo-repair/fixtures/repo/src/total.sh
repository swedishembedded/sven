# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>

# Order total in cents: items plus shipping, less the discount.
total() {
  items=$1
  shipping=$2
  discount=$3
  echo $(( items - discount ))
}
