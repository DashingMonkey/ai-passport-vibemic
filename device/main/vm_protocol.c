// main/vm_protocol.c —— VibeMic 协议帧编解码实现。纯逻辑,host tests 覆盖。
#include "vm_protocol.h"

#include <string.h>

uint16_t vm_crc16(const uint8_t *data, size_t len)
{
    uint16_t crc = 0xFFFF;
    for (size_t i = 0; i < len; i++) {
        crc ^= (uint16_t)data[i] << 8;
        for (int bit = 0; bit < 8; bit++) {
            crc = (crc & 0x8000) ? (uint16_t)((crc << 1) ^ 0x1021) : (uint16_t)(crc << 1);
        }
    }
    return crc;
}

size_t vm_frame_encode(uint8_t *buf, size_t cap, uint8_t version, uint8_t type,
                       const uint8_t *payload, uint16_t len)
{
    // 帧总长 = 头 6B(magic2+ver1+type1+len2) + payload + CRC 2B。
    const size_t total = 8u + len;
    if (!buf || cap < total) return 0;

    buf[0] = VM_PROTO_MAGIC0;
    buf[1] = VM_PROTO_MAGIC1;
    buf[2] = version;
    buf[3] = type;
    buf[4] = (uint8_t)(len & 0xFF);
    buf[5] = (uint8_t)(len >> 8);
    if (len && payload) memcpy(&buf[6], payload, len);

    const uint16_t crc = vm_crc16(&buf[2], 4u + len);
    buf[6 + len] = (uint8_t)(crc & 0xFF);
    buf[7 + len] = (uint8_t)(crc >> 8);
    return total;
}

int vm_frame_verify(const uint8_t *buf, size_t len)
{
    if (!buf || len < 8 || len > VM_FRAME_MAX + 8u) return -1;
    if (buf[0] != VM_PROTO_MAGIC0 || buf[1] != VM_PROTO_MAGIC1) return -1;
    const uint16_t plen = (uint16_t)(buf[4] | (buf[5] << 8));
    if ((size_t)plen + 8u != len) return -1;
    const uint16_t crc = (uint16_t)(buf[6 + plen] | (buf[7 + plen] << 8));
    return vm_crc16(&buf[2], 4u + plen) == crc ? 0 : -1;
}

void vm_decoder_init(vm_decoder_t *d)
{
    memset(d, 0, sizeof(*d));
}

static uint16_t decoder_payload_len(const vm_decoder_t *d)
{
    return (uint16_t)(d->buf[4] | (d->buf[5] << 8));
}

// 判定当前缓冲:0=继续收,1=完整一帧(视图写入 d->frame 并复位缓冲),
// -1=帧错误(magic 不符/超长/CRC 错),缓冲已复位,继续喂后续字节。
static int decoder_try_frame(vm_decoder_t *d)
{
    if (d->fill < 6) {
        if (d->fill >= 2 && (d->buf[0] != VM_PROTO_MAGIC0 || d->buf[1] != VM_PROTO_MAGIC1)) {
            // 保留可能属于下一个 magic 的尾字节,不能按两字节成对丢弃。
            const uint8_t tail = d->buf[d->fill - 1];
            d->fill = (tail == VM_PROTO_MAGIC0) ? 1 : 0;
            if (d->fill) d->buf[0] = tail;
            return -1;
        }
        return 0;
    }
    if (d->buf[0] != VM_PROTO_MAGIC0 || d->buf[1] != VM_PROTO_MAGIC1) {
        d->fill = 0;
        return -1;
    }
    if (d->need == 0) {
        const uint16_t plen = decoder_payload_len(d);
        if (plen > VM_FRAME_MAX) {
            d->fill = 0;
            return -1;
        }
        d->need = 8u + plen;
    }
    if ((uint32_t)d->fill < d->need) return 0;

    if (vm_frame_verify(d->buf, d->need) != 0) {
        d->fill = 0;
        d->need = 0;
        return -1;
    }
    d->frame.version = d->buf[2];
    d->frame.type = d->buf[3];
    d->frame.len = decoder_payload_len(d);
    d->frame.payload = d->buf + 6;
    d->fill = 0; // 缓冲内容保留到下一帧覆盖前,视图仍可读。
    d->need = 0;
    return 1;
}

int vm_decoder_feed(vm_decoder_t *d, uint8_t byte, const vm_frame_t **out)
{
    if (d->fill >= sizeof(d->buf)) {
        d->fill = 0; // 无效数据塞满缓冲:清空等下一个 magic。
        d->need = 0;
    }
    d->buf[d->fill++] = byte;
    const int rc = decoder_try_frame(d);
    if (rc == 1 && out) *out = &d->frame;
    return rc;
}

// ---- 载荷编解码(小端) ----

size_t vm_payload_encode_status(uint8_t *buf, size_t cap, const vm_proto_status_t *st)
{
    if (!buf || !st || cap < 11) return 0;
    buf[0] = (uint8_t)(st->fw_ver & 0xFF);
    buf[1] = (uint8_t)(st->fw_ver >> 8);
    buf[2] = st->battery_pct;
    buf[3] = st->charging;
    buf[4] = st->conn_state;
    buf[5] = st->active_profile;
    buf[6] = st->bond_slot;
    buf[7] = (uint8_t)(st->free_heap >> 0);
    buf[8] = (uint8_t)(st->free_heap >> 8);
    buf[9] = (uint8_t)(st->free_heap >> 16);
    buf[10] = (uint8_t)(st->free_heap >> 24);
    return 11;
}

int vm_payload_decode_status(const uint8_t *buf, size_t len, vm_proto_status_t *st)
{
    if (!buf || !st || len != 11) return -1;
    st->fw_ver = (uint16_t)(buf[0] | (buf[1] << 8));
    st->battery_pct = buf[2];
    st->charging = buf[3];
    st->conn_state = buf[4];
    st->active_profile = buf[5];
    st->bond_slot = buf[6];
    st->free_heap = (uint32_t)buf[7] | ((uint32_t)buf[8] << 8) |
                    ((uint32_t)buf[9] << 16) | ((uint32_t)buf[10] << 24);
    return 0;
}

size_t vm_payload_encode_result(uint8_t *buf, size_t cap, const vm_proto_result_t *r)
{
    if (!buf || !r || cap < 2) return 0;
    buf[0] = r->req_type;
    buf[1] = r->code;
    return 2;
}

int vm_payload_decode_result(const uint8_t *buf, size_t len, vm_proto_result_t *r)
{
    if (!buf || !r || len != 2) return -1;
    r->req_type = buf[0];
    r->code = buf[1];
    return 0;
}

size_t vm_payload_encode_button_event(uint8_t *buf, size_t cap,
                                      const vm_proto_button_event_t *ev)
{
    if (!buf || !ev || cap < 6) return 0;
    buf[0] = ev->button;
    buf[1] = ev->event;
    buf[2] = (uint8_t)(ev->ts_ms >> 0);
    buf[3] = (uint8_t)(ev->ts_ms >> 8);
    buf[4] = (uint8_t)(ev->ts_ms >> 16);
    buf[5] = (uint8_t)(ev->ts_ms >> 24);
    return 6;
}

int vm_payload_decode_button_event(const uint8_t *buf, size_t len,
                                   vm_proto_button_event_t *ev)
{
    if (!buf || !ev || len != 6) return -1;
    ev->button = buf[0];
    ev->event = buf[1];
    ev->ts_ms = (uint32_t)buf[2] | ((uint32_t)buf[3] << 8) |
                ((uint32_t)buf[4] << 16) | ((uint32_t)buf[5] << 24);
    return 0;
}

int vm_payload_decode_chunk(const uint8_t *buf, size_t len, vm_proto_chunk_t *ch)
{
    if (!buf || !ch || len < 6) return -1;
    ch->profile_id = buf[0];
    ch->seq = (uint16_t)(buf[1] | (buf[2] << 8));
    ch->total = (uint16_t)(buf[3] | (buf[4] << 8));
    ch->data_len = buf[5];
    ch->data = buf + 6;
    if ((size_t)ch->data_len + 6u != len) return -1;
    return 0;
}
