#!/bin/sh
set -eu
exec node --inspect=127.0.0.1:9229 app.js
