IMAGE     := swedishembedded/sven-site
# The image carries the site's version, which is sven's (release.toml sets
# it with every sven release), so the host pins exactly what it runs.
VERSION   := $(shell node -p "require('./package.json').version")
PORT      := 3000
DOCKER    ?= sudo docker
NO_CACHE  ?= 0

DOCKER_BUILD_FLAGS :=
ifeq ($(NO_CACHE),1)
DOCKER_BUILD_FLAGS += --no-cache
endif

.PHONY: build serve install

## build          - build the Docker image (swedishembedded/sven-site:v<package.json version>)
##                  use NO_CACHE=1 to rebuild without layer cache
build: install
	npm run build
	$(DOCKER) build $(DOCKER_BUILD_FLAGS) \
		--label org.opencontainers.image.version=$(VERSION) \
		--label org.opencontainers.image.revision=$$(git rev-parse HEAD) \
		-t $(IMAGE):v$(VERSION) .

## serve          - run the Vite dev server locally on http://localhost:$(PORT)
serve: install
	npm run dev -- --port $(PORT)

## install        - install npm dependencies
install:
	npm install
