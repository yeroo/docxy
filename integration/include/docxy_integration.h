/* The C ABI of an integration plugin, interface version 1.
 * docs/integration-plugins.md in the docxy repository is the contract. Its
 * answers' fields are all optional, so a field it adds keeps the interface
 * version. */
#ifndef DOCXY_INTEGRATION_H
#define DOCXY_INTEGRATION_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define DXI_ABI_VERSION 1

#define DXI_OK 0
#define DXI_UNKNOWN_METHOD 1
#define DXI_MALFORMED_REQUEST 2
#define DXI_NOT_CONFIGURED 3
#define DXI_INTERNAL_ERROR 4

/* A buffer the plugin owns. The host passes {NULL, 0}, copies what the plugin
 * put in it, and gives it back through dxi_free. */
typedef struct {
    uint8_t *ptr;
    size_t len;
} DxiBuf;

uint32_t dxi_abi(void);
int32_t dxi_init(const uint8_t *host_json, size_t len, DxiBuf *out);
int32_t dxi_call(const char *method, const uint8_t *req_json, size_t len, DxiBuf *out);
void dxi_free(DxiBuf *buf);
int32_t dxi_last_error(DxiBuf *utf8_out);

#ifdef __cplusplus
}
#endif

#endif
