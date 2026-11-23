# deploy.sh builds the binary on the host, so the base image must match the host's glibc.
FROM ubuntu:26.04
RUN useradd -r -u 10001 kairos
COPY kairos-server /usr/local/bin/kairos-server
USER kairos
ENV BIND=0.0.0.0:8787
EXPOSE 8787
CMD ["kairos-server"]
