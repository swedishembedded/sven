# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>

. src/total.sh
status=0
check() {
  actual=$(total "$2" "$3" "$4")
  if [ "$actual" != "$5" ]; then
    echo "FAIL $1: total $2 $3 $4 = $actual, expected $5"
    status=1
  else
    echo "ok $1"
  fi
}
check no_shipping 1000 0 0 1000
check with_shipping 1000 250 0 1250
check with_discount 1000 250 100 1150
exit $status
