FROM ubuntu:24.04
RUN apt-get update -qq \
 && apt-get install -y -qq --no-install-recommends python3 python-is-python3 git ca-certificates >/dev/null \
 && rm -rf /var/lib/apt/lists/*
