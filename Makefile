#Rust + Docker
PACKAGE_VERSION := $(shell cargo metadata --manifest-path ./Cargo.toml --no-deps --format-version 1 | jq -r '.packages[] | select( .targets[].kind[] | contains("bin") ) | .version')
PACKAGE_NAME := $(shell cargo metadata --manifest-path ./Cargo.toml --no-deps --format-version 1 | jq -r '.packages[] | select( .targets[].kind[] | contains("bin") ) | .name')

.PHONY: all deploy clean

all: build deploy

.check_env:
	if [[ -z "${REGISTRY}" ]]; then echo "Please provide env REGISTRY with domain to container registry"; exit 1; fi

clean:
	cargo clean

build: .check_env
	docker build --platform linux/amd64 -f Dockerfile -t $(PACKAGE_NAME) -t ${REGISTRY}/$(PACKAGE_NAME):$(PACKAGE_VERSION) .

deploy: .check_env
	sudo docker push ${REGISTRY}/$(PACKAGE_NAME):$(PACKAGE_VERSION)
