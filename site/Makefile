IMAGE     := swedishembedded/sven-site
# The image carries the site's version, so the host pins exactly what it
# runs (a moving `latest` changes production without a commit there).
VERSION   := $(shell node -p "require('./package.json').version")
PORT      := 3000
DOCKER    ?= sudo docker
NO_CACHE  ?= 0

DOCKER_BUILD_FLAGS :=
ifeq ($(NO_CACHE),1)
DOCKER_BUILD_FLAGS += --no-cache
endif

.PHONY: build serve release install

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

## release        - bump the version (PART=patch|minor|major), commit it and tag
##                  site-v<version>, locally; the infrastructure repository's
##                  release controller builds, deploys and promotes the image
release:
	@case "$(PART)" in patch|minor|major) ;; *) echo "release: PART=patch|minor|major" >&2; exit 2 ;; esac
	@[ -z "$$(git status --porcelain -- .)" ] || { echo "release: the site has uncommitted changes" >&2; git status --short -- .; exit 1; }
	npm version $(PART) --no-git-tag-version >/dev/null
	git commit --quiet --only package.json \
		-m "site: release $$(node -p "require('./package.json').version")"
	git tag "site-v$$(node -p "require('./package.json').version")"
	@echo "Tagged site-v$$(node -p "require('./package.json').version") (push with git push --follow-tags when ready)"

## install        - install npm dependencies
install:
	npm install
