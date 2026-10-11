# Public source boundary

Published source includes the implemented code, reproducible build inputs, tests and original license notices. Private credentials, signing keys, personal dictionaries, input/conversation history, downloaded corpora, unreviewed training checkpoints and local settings are excluded. They were not uploaded to a separate private repository.

Public statistical models in Luqnexa-AI were built from the attributed public sources identified in models/. Downloaded neural models and executable runtimes are fetched separately from pinned official sources and checksum-verified.

Technical documents retain historical monorepo paths and measurements. In the split repositories, shared dictionary types live in NEXC, AI/ranking code and training scripts live in Luqnexa-AI, and the public collector lives in Luqnexa-Server. The IME and collector pin those components through Git submodules. The current entry point is each repository's README.

The dictionary distribution API and free-form AI text generation are not implemented. No complete-training, general-accuracy, 100-million-entry or iPhone-SE performance claim is made.
