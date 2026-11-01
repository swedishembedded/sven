IMAGE     := swedishembedded/sven-site
# The image carries the site's version, so the host pins exactly what it
# runs (a moving `latest` changes production without a commit there).
VERSION   := $(shell node -p "require('./package.json').version")
PORT      := 3000
DOCKER    ?= sudo docker
# The edge host that serves agentsven.com, by the name SSH knows it as.
TARGET    ?= swedishembedded.com
NO_CACHE  ?= 0

DOCKER_BUILD_FLAGS :=
ifeq ($(NO_CACHE),1)
DOCKER_BUILD_FLAGS += --no-cache
endif

.PHONY: build serve publish install

## build          - build the Docker image (swedishembedded/sven-site:v<package.json version>)
##                  use NO_CACHE=1 to rebuild without layer cache
build: install
	npm run build
	$(DOCKER) build $(DOCKER_BUILD_FLAGS) -t $(IMAGE):v$(VERSION) .

## serve          - run the Vite dev server locally on http://localhost:$(PORT)
serve: install
	npm run dev -- --port $(PORT)

## publish        - upload the Docker image to the edge host ($(TARGET)) via SSH
publish: build
	@echo "Publishing $(IMAGE):v$(VERSION) to $(TARGET)..."
	$(DOCKER) save $(IMAGE):v$(VERSION) | ssh $(TARGET) docker load
	@echo "Done. Release it with portal_agentsven_version: v$(VERSION) in the infrastructure repository."

## install        - install npm dependencies
install:
	npm install
