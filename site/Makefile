IMAGE   := swedishembedded/sven-site
PORT    := 3000
DOCKER  ?= sudo docker

.PHONY: build serve publish install copy-install

## build          – build the Docker image (swedishembedded/sven-site)
build: install copy-install
	npm run build
	$(DOCKER) build -t $(IMAGE) .

## serve          – run the Vite dev server locally on http://localhost:$(PORT)
serve: install copy-install
	npm run dev -- --port $(PORT)

## publish        – upload the Docker image to martin@swedishembedded.com via SSH
publish: build
	@echo "Publishing $(IMAGE) to swedishembedded.com..."
	$(DOCKER) save $(IMAGE) | ssh martin@swedishembedded.com docker load
	@echo "Done. Infrastructure on swedishembedded.com will handle deployment."

## install        – install npm dependencies
install:
	npm install

## copy-install   – copy ../scripts/install.sh → public/install (served at /install)
copy-install:
	cp ../scripts/install.sh public/install
	chmod +x public/install
