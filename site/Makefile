IMAGE     := swedishembedded/sven-site
PORT      := 3000
DOCKER    ?= sudo docker
TARGET    ?= agentsven.com
NO_CACHE  ?= 0

DOCKER_BUILD_FLAGS :=
ifeq ($(NO_CACHE),1)
DOCKER_BUILD_FLAGS += --no-cache
endif

.PHONY: build serve publish install copy-install

## build          – build the Docker image (swedishembedded/sven-site)
##                  use NO_CACHE=1 to rebuild without layer cache
build: install copy-install
	npm run build
	$(DOCKER) build $(DOCKER_BUILD_FLAGS) -t $(IMAGE) .

## serve          – run the Vite dev server locally on http://localhost:$(PORT)
serve: install copy-install
	npm run dev -- --port $(PORT)

## publish        – upload the Docker image to martin@swedishembedded.com via SSH
publish: build
	@echo "Publishing $(IMAGE) to $(TARGET)..."
	$(DOCKER) save $(IMAGE) | ssh $(TARGET) docker load
	@echo "Done. Infrastructure on $(TARGET) will handle deployment."

## install        – install npm dependencies
install:
	npm install

## copy-install   – copy ../scripts/install.sh → public/install.sh (served at /install.sh)
copy-install:
	cp ../scripts/install.sh public/install.sh
	chmod +x public/install.sh
