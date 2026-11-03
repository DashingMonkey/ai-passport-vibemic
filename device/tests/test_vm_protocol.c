// tests/test_vm_protocol.c —— VibeMic 协议帧编解码 host tests(纯逻辑,无 SDK)。
// 编译运行方式与 validate.sh 其余 host tests 一致:
//   cc -std=c11 -Wall -Wextra -Werror -Imain tests/test_vm_protocol.c main/vm_protocol.c
#include <assert.h>
#include <stdio.h>
#include <string.h>

#include "vm_protocol.h"

// CRC16-CCITT (init 0xFFFF) 已知向量:"123456789" → 0x29B1 (CRC-FALSE 标准检验值)。
static void test_crc16_known_vector(void)
{
    const uint8_t data[] = "123456789";
    assert(vm_crc16(data, 9) == 0x29B1);
    assert(vm_crc16(data, 0) == 0xFFFF);
}

static void test_encode_verify_roundtrip(void)
{
    uint8_t buf[64];
    const uint8_t payload[] = { 0xAB, 0xCD, 0xEF };

    const size_t n = vm_frame_encode(buf, sizeof(buf), VM_PROTO_VERSION,
                                     VM_FRAME_CMD_SET_ACTIVE, payload, 3);
    assert(n == 11);
    assert(buf[0] == 0x56 && buf[1] == 0x4D && buf[2] == 1 && buf[3] == 0x01);
    assert(buf[4] == 3 && buf[5] == 0); // len u16 LE
    assert(buf[6] == 0xAB && buf[7] == 0xCD && buf[8] == 0xEF);
    assert(vm_frame_verify(buf, n) == 0);

    // 空 payload 帧。
    const size_t m = vm_frame_encode(buf, sizeof(buf), VM_PROTO_VERSION,
                                     VM_FRAME_CMD_GET_STATUS, NULL, 0);
    assert(m == 8);
    assert(vm_frame_verify(buf, m) == 0);
}

static void test_encode_rejects(void)
{
    uint8_t buf[8];
    assert(vm_frame_encode(buf, sizeof(buf), 1, 0x01, NULL, 3) == 0); // cap 不足
    uint8_t big[VM_FRAME_MAX + 1] = { 0 };
    uint8_t out[VM_FRAME_MAX + 8];
    assert(vm_frame_encode(out, sizeof(out), 1, 0x04, big, VM_FRAME_MAX) ==
           VM_FRAME_MAX + 8u); // 恰好上限可用
    assert(vm_frame_encode(NULL, 16, 1, 0x01, NULL, 0) == 0);
}

static void test_verify_detects_damage(void)
{
    uint8_t buf[16];
    const size_t n = vm_frame_encode(buf, sizeof(buf), 1, 0x01,
                                     (const uint8_t[]){ 1 }, 1);
    assert(n == 9);
    buf[6] ^= 0x01; // payload 翻位
    assert(vm_frame_verify(buf, n) == -1);
    buf[6] ^= 0x01;
    buf[7] ^= 0x01; // CRC 翻位
    assert(vm_frame_verify(buf, n) == -1);
    buf[7] ^= 0x01;
    buf[0] = 0x00; // magic 破坏
    assert(vm_frame_verify(buf, n) == -1);
    assert(vm_frame_verify(buf, n - 1) == -1); // 长度不符
}

static void test_decoder_stream(void)
{
    uint8_t frame[16];
    const uint8_t payload[] = { 0xDE, 0xAD, 0xBE, 0xEF };
    const size_t n = vm_frame_encode(frame, sizeof(frame), 1, VM_FRAME_CMD_DISCONNECT,
                                     payload, 4);
    assert(n == 12);

    vm_decoder_t d;
    vm_decoder_init(&d);
    const vm_frame_t *view = NULL;

    // 前置噪声:第二字节即可判定 magic 不符 → -1 且复位。
    assert(vm_decoder_feed(&d, 0x00, &view) == 0);
    assert(vm_decoder_feed(&d, 0x00, &view) == -1);

    // 完整帧逐字节喂入,仅最后一字节交付。
    for (size_t i = 0; i < n; i++) {
        const int rc = vm_decoder_feed(&d, frame[i], &view);
        if (i + 1 == n) {
            assert(rc == 1);
            assert(view->type == VM_FRAME_CMD_DISCONNECT);
            assert(view->len == 4);
            assert(memcmp(view->payload, payload, 4) == 0);
        } else {
            assert(rc == 0);
        }
    }

    // 连续两帧背靠背,复用同一解码器。
    for (int round = 0; round < 2; round++) {
        for (size_t i = 0; i < n; i++) {
            const int rc = vm_decoder_feed(&d, frame[i], &view);
            if (i + 1 == n) assert(rc == 1);
            else assert(rc == 0);
        }
    }
}

static void test_decoder_recovers_from_garbage(void)
{
    vm_decoder_t d;
    vm_decoder_init(&d);
    const vm_frame_t *view = NULL;

    // 帧中 CRC 损坏 → -1 且复位;随后好帧仍可交付。
    uint8_t bad[16];
    const size_t n = vm_frame_encode(bad, sizeof(bad), 1, 0x02, NULL, 0);
    assert(n == 8);
    bad[6] ^= 0xFF;
    int saw_error = 0;
    for (size_t i = 0; i < n; i++) {
        const int rc = vm_decoder_feed(&d, bad[i], &view);
        if (rc == -1) saw_error = 1;
    }
    assert(saw_error);

    uint8_t good[16];
    const size_t g = vm_frame_encode(good, sizeof(good), 1, 0x02, NULL, 0);
    for (size_t i = 0; i < g; i++) {
        const int rc = vm_decoder_feed(&d, good[i], &view);
        if (i + 1 == g) {
            assert(rc == 1);
            assert(view->type == VM_FRAME_CMD_GET_STATUS);
        } else {
            assert(rc == 0);
        }
    }

    // 超长 len 声明被拒绝。
    vm_decoder_init(&d);
    const uint8_t hostile[] = { 0x56, 0x4D, 0x01, 0x04, 0xFF, 0xFF };
    for (size_t i = 0; i < sizeof(hostile); i++) {
        vm_decoder_feed(&d, hostile[i], &view);
    }
    assert(d.need == 0); // 已复位,未定形为超长帧
}

static void test_status_payload(void)
{
    const vm_proto_status_t src = {
        .fw_ver = 0x0102,
        .battery_pct = 87,
        .charging = 0,
        .conn_state = VM_CONN_CONNECTED,
        .active_profile = 2,
        .bond_slot = VM_BOND_A,
        .free_heap = 0x12345678,
    };
    uint8_t buf[16];
    assert(vm_payload_encode_status(buf, sizeof(buf), &src) == 11);

    vm_proto_status_t dst;
    assert(vm_payload_decode_status(buf, 11, &dst) == 0);
    assert(dst.fw_ver == 0x0102 && dst.battery_pct == 87 && dst.charging == 0);
    assert(dst.conn_state == VM_CONN_CONNECTED && dst.active_profile == 2);
    assert(dst.bond_slot == VM_BOND_A && dst.free_heap == 0x12345678);

    // 固定字节序:free_heap 低字节在前。
    assert(buf[7] == 0x78 && buf[10] == 0x12);
    assert(vm_payload_encode_status(buf, 10, &src) == 0); // cap 不足
    assert(vm_payload_decode_status(buf, 10, &dst) == -1); // 长度不符
}

static void test_result_and_event_payload(void)
{
    uint8_t buf[8];
    const vm_proto_result_t r = { .req_type = VM_FRAME_CMD_SET_ACTIVE,
                                  .code = VM_RESULT_STORAGE };
    assert(vm_payload_encode_result(buf, sizeof(buf), &r) == 2);
    vm_proto_result_t r2;
    assert(vm_payload_decode_result(buf, 2, &r2) == 0);
    assert(r2.req_type == 0x01 && r2.code == 3);
    assert(vm_payload_decode_result(buf, 1, &r2) == -1);

    const vm_proto_button_event_t ev = { .button = 2, .event = VM_BTN_EV_RELEASE,
                                         .ts_ms = 0x01020304 };
    assert(vm_payload_encode_button_event(buf, sizeof(buf), &ev) == 6);
    vm_proto_button_event_t ev2;
    assert(vm_payload_decode_button_event(buf, 6, &ev2) == 0);
    assert(ev2.button == 2 && ev2.event == 1 && ev2.ts_ms == 0x01020304);
}

static void test_chunk_payload(void)
{
    uint8_t payload[10] = { 3, 0x02, 0x00, 0x05, 0x00, 4, 'a', 'b', 'c', 'd' };
    vm_proto_chunk_t ch;
    assert(vm_payload_decode_chunk(payload, 10, &ch) == 0);
    assert(ch.profile_id == 3 && ch.seq == 2 && ch.total == 5 && ch.data_len == 4);
    assert(memcmp(ch.data, "abcd", 4) == 0);
    assert(vm_payload_decode_chunk(payload, 9, &ch) == -1); // dataLen 与长度不符
    assert(vm_payload_decode_chunk(payload, 5, &ch) == -1); // 头都不够
}

int main(void)
{
    // Odd noise and overlapping magic must not lock the parser out of every frame.
    const uint8_t prefixes[][3] = {{0}, {0x56}, {0, 0, 0}, {0x56, 0x56}};
    const size_t lengths[] = {1, 1, 3, 2};
    for (size_t p = 0; p < 4; p++) {
        vm_decoder_t d;
        vm_decoder_init(&d);
        const vm_frame_t *out = NULL;
        for (size_t i = 0; i < lengths[p]; i++) vm_decoder_feed(&d, prefixes[p][i], &out);
        uint8_t frame[8];
        vm_frame_encode(frame, sizeof(frame), 1, VM_FRAME_CMD_GET_STATUS, NULL, 0);
        for (int round = 0; round < 20; round++) {
            int count = 0;
            for (size_t i = 0; i < sizeof(frame); i++) count += vm_decoder_feed(&d, frame[i], &out) == 1;
            assert(count == 1 && out->type == VM_FRAME_CMD_GET_STATUS);
        }
    }
    test_crc16_known_vector();
    test_encode_verify_roundtrip();
    test_encode_rejects();
    test_verify_detects_damage();
    test_decoder_stream();
    test_decoder_recovers_from_garbage();
    test_status_payload();
    test_result_and_event_payload();
    test_chunk_payload();
    puts("vm_protocol host tests: PASS");
    return 0;
}
