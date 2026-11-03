// 语法检查桩:内容统一在 idf_stubs.h(见该文件头的说明)。
#pragma once
#include "idf_stubs.h"

// bsp_i2c.h 需要 I2C master 句柄类型(真实定义在 ESP-IDF driver/i2c_master.h)。
typedef struct i2c_master_bus_t *i2c_master_bus_handle_t;
typedef struct i2c_master_dev_t *i2c_master_dev_handle_t;
