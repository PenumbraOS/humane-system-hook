use fastembed::{
    EmbeddingModel as FastembedModel, Pooling, TextEmbedding as FastembedTextEmbedding,
    TokenizerFiles, UserDefinedEmbeddingModel,
};
use rig_fastembed::EmbeddingModel;

const MODEL_DIMENSIONS: usize = 384;

pub fn build_embedding_model() -> Result<EmbeddingModel, String> {
    let tokenizer_files = TokenizerFiles {
        tokenizer_file: include_bytes!(concat!(
            env!("OUT_DIR"),
            "/embedded-embedding-model/tokenizer.json"
        ))
        .to_vec(),
        config_file: include_bytes!(concat!(env!("OUT_DIR"), "/embedded-embedding-model/config.json"))
            .to_vec(),
        special_tokens_map_file: include_bytes!(concat!(
            env!("OUT_DIR"),
            "/embedded-embedding-model/special_tokens_map.json"
        ))
        .to_vec(),
        tokenizer_config_file: include_bytes!(concat!(
            env!("OUT_DIR"),
            "/embedded-embedding-model/tokenizer_config.json"
        ))
        .to_vec(),
    };

    let user_defined_model = UserDefinedEmbeddingModel::new(
        include_bytes!(concat!(env!("OUT_DIR"), "/embedded-embedding-model/model.onnx")).to_vec(),
        tokenizer_files,
    )
    .with_pooling(Pooling::Mean);

    let model_info = FastembedTextEmbedding::get_model_info(&FastembedModel::AllMiniLML6V2)
        .map_err(|err| err.to_string())?;

    EmbeddingModel::new_from_user_defined(user_defined_model, MODEL_DIMENSIONS, model_info)
        .map_err(|err| err.to_string())
}
