# VibeMic 衍生应用(fork 规则:基线 demo 源码保留在 main/ 供上游 host tests 使用,
# 但不参与本应用构建;UI 为全新设计,未复用 demo 外壳)。
idf_component_register(
    SRCS "vibemic_app.c"
         "vm_audio.c"
         "vm_audio_codec.c"
         "vm_backlight.c"
         "vm_batt_icon.c"
         "vm_buttons.c"
         "vm_hid.c"
         "vm_keymap.c"
         "vm_link.c"
         "vm_profile.c"
         "vm_protocol.c"
    INCLUDE_DIRS "."
    REQUIRES bsp bt nvs_flash esp_timer
)
