use hc_tag_system::TagSystemManager;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🏷️ 测试标签系统");

    // 设置测试工作空间
    let workspace_root =
        PathBuf::from("/mnt/code_disk/code/rust/honeycomb/workspace/tenants/local/users/default");

    // 创建标签系统管理器
    let mut manager = TagSystemManager::new(workspace_root);

    println!("📁 初始化标签系统...");
    if let Err(e) = manager.initialize() {
        println!("⚠️ 初始化失败: {}", e);
        return Ok(());
    }

    println!("✅ 标签系统初始化成功!");

    // 显示加载的维度
    println!("\n📊 已加载的维度:");
    for (id, dimension) in manager.get_dimensions() {
        println!("  - {} ({}): {}", id, dimension.name, dimension.description);
        println!("    低: {:?}", dimension.keywords.low);
        println!("    中: {:?}", dimension.keywords.medium);
        println!("    高: {:?}", dimension.keywords.high);
    }

    // 显示加载的标签
    println!("\n🏷️ 已加载的标签:");
    for (id, tag) in manager.get_tags() {
        println!(
            "  - {} ({}): {} = {:.2}",
            id, tag.dimension, tag.name, tag.value
        );
    }

    // 测试输入分析
    println!("\n🔍 测试输入分析:");
    let test_inputs = [
        "创建一个复杂的算法来优化系统性能",
        "复制文件到新目录",
        "设计一个创新的用户界面",
        "紧急修复数据库连接问题",
    ];

    for input in &test_inputs {
        println!("\n输入: \"{}\"", input);
        let tags = manager.analyze_input_tags(input);
        for (dimension, score) in &tags.dimensions {
            println!("  {} = {:.2}", dimension, score);
        }
    }

    // 测试相似度计算
    println!("\n🎯 测试实体相似度:");
    let query_tags = manager.analyze_input_tags("我需要生成复杂的代码");

    // 测试与code_generation工具的相似度
    let similarity = manager.calculate_entity_similarity(&query_tags, "code_generation", "tools");
    println!(
        "查询 \"我需要生成复杂的代码\" 与 code_generation 工具的相似度: {:.2}",
        similarity
    );

    // 测试与file_operations工具的相似度
    let similarity = manager.calculate_entity_similarity(&query_tags, "file_operations", "tools");
    println!(
        "查询 \"我需要生成复杂的代码\" 与 file_operations 工具的相似度: {:.2}",
        similarity
    );

    println!("\n🎉 标签系统测试完成!");
    Ok(())
}
