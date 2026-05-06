#!/bin/bash

cd /mnt/code_disk/code/rust/honeycomb

echo "🧪 测试配置检查修复效果"
echo "================================"
echo

echo "预期结果:"
echo "  ✅ 应该显示: 'API密钥: HC_LLM_API_KEY'"
echo "  ✅ 应该显示: 'Base URL: HC_LLM_BASE_URL'"
echo "  ❌ 不应该显示: '缺少必需配置'"
echo

echo "实际运行结果:"
# 使用timeout确保不会卡住，并且只显示配置检查部分
timeout 15s ./target/debug/hc-cli 2>&1 | head -10

echo
echo "💡 解释:"
echo "现在系统会检查通用API密钥 HC_LLM_API_KEY (优先)"
echo "然后才检查提供商专用密钥 DEEPSEEK_API_KEY"
echo "这与 hc-llm 的实际逻辑保持一致！"