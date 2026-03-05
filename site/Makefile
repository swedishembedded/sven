IMAGE   := swedishembedded/sven-site
PORT    := 3000
DOCKER  ?= docker

.PHONY: build serve publish install

## build   – build the Docker image (swedishembedded/sven-site)
build: install
	npm run build
	$(DOCKER) build -t $(IMAGE) .

## serve   – run the Vite dev server locally on http://localhost:$(PORT)
serve: install
	npm run dev -- --port $(PORT)

## publish – upload the Docker image to martin@swedishembedded.com via SSH
publish: build
	@echo "Publishing $(IMAGE) to swedishembedded.com..."
	$(DOCKER) save $(IMAGE) | ssh martin@swedishembedded.com docker load
	@echo "Done. Infrastructure on swedishembedded.com will handle deployment."

## install – install npm dependencies
install:
	npm install
