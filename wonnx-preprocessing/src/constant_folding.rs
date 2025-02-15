use std::{borrow::Cow, collections::HashMap};

use protobuf::{ProtobufEnum, RepeatedField};
use thiserror::Error;

use wonnx::{
    onnx::{
        GraphProto, NodeProto, TensorProto, TensorShapeProto, TensorShapeProto_Dimension,
        TypeProto, TypeProto_Tensor, ValueInfoProto,
    },
    utils::{
        model_with_opset, DataTypeError, InputTensor, NodeAttributes, OutputTensor, ScalarType,
        Shape,
    },
    CompileError, GpuError, Session, SessionError,
};

#[derive(Error, Debug)]
pub enum ConstantFoldingError {
    #[error("unsupported data type encountered: {0}")]
    #[from(DataTypeError)]
    UnsupportedDataType(DataTypeError),

    #[error("invalid node: {0}")]
    InvalidNode(String),

    #[error("error calculating constant value: {0}")]
    #[from(SessionError)]
    CalculationError(SessionError),
}

pub(crate) async fn calculate_constant_node_outputs<'a>(
    node: &'a NodeProto,
    shapes: &'a HashMap<String, Shape>,
    inputs: &'a [InputTensor<'a>],
    output_shapes: &[Shape],
    _initializers: &HashMap<String, Cow<'a, TensorProto>>,
    opset_version: i64,
) -> Result<Option<Vec<OutputTensor>>, ConstantFoldingError> {
    Ok(match node.get_op_type() {
        "Identity" | "Unsqueeze" | "Squeeze" | "Reshape" => {
            Some(inputs.iter().map(OutputTensor::from).collect())
        }
        "Cast" => {
            let cast_to_type =
                ScalarType::from_i32(node.get_attribute_value::<i64>("to", None).map_err(|_| {
                    ConstantFoldingError::InvalidNode("to attribute missing for Cast ".to_string())
                })? as i32)
                .map_err(ConstantFoldingError::UnsupportedDataType)?;
            let input_tensor = &inputs[0];

            let output_tensor = match (input_tensor, cast_to_type) {
                (InputTensor::F32(v), ScalarType::F32) => OutputTensor::F32(v.to_vec()),
                (InputTensor::F32(v), ScalarType::I64) => {
                    OutputTensor::I64(v.iter().map(|x| *x as i64).collect())
                }
                (InputTensor::F32(v), ScalarType::I32) => {
                    OutputTensor::I32(v.iter().map(|x| *x as i32).collect())
                }
                (InputTensor::F32(v), ScalarType::U8) => {
                    OutputTensor::U8(v.iter().map(|x| *x as u8).collect())
                }
                (InputTensor::I32(v), ScalarType::F32) => {
                    OutputTensor::F32(v.iter().map(|x| *x as f32).collect())
                }
                (InputTensor::I32(v), ScalarType::I64) => {
                    OutputTensor::I64(v.iter().map(|x| *x as i64).collect())
                }
                (InputTensor::I32(v), ScalarType::I32) => OutputTensor::I32(v.to_vec()),
                (InputTensor::I32(v), ScalarType::U8) => {
                    OutputTensor::U8(v.iter().map(|x| *x as u8).collect())
                }
                (InputTensor::I64(v), ScalarType::F32) => {
                    OutputTensor::F32(v.iter().map(|x| *x as f32).collect())
                }
                (InputTensor::I64(v), ScalarType::I64) => OutputTensor::I64(v.to_vec()),
                (InputTensor::I64(v), ScalarType::I32) => {
                    OutputTensor::I32(v.iter().map(|x| *x as i32).collect())
                }
                (InputTensor::I64(v), ScalarType::U8) => {
                    OutputTensor::U8(v.iter().map(|x| *x as u8).collect())
                }
                (InputTensor::U8(v), ScalarType::F32) => {
                    OutputTensor::F32(v.iter().map(|x| *x as f32).collect())
                }
                (InputTensor::U8(v), ScalarType::I64) => {
                    OutputTensor::I64(v.iter().map(|x| *x as i64).collect())
                }
                (InputTensor::U8(v), ScalarType::I32) => {
                    OutputTensor::I32(v.iter().map(|x| *x as i32).collect())
                }
                (InputTensor::U8(v), ScalarType::U8) => OutputTensor::U8(v.to_vec()),
            };

            Some(vec![output_tensor])
        }

        // Shape: produces an output containing the shape of the input tensor
        "Shape" => {
            let input_shape = &shapes[&node.input[0]];
            Some(vec![calculate_shape_operator(node, input_shape)?])
        }

        // ConstantOfShape: produces an output of the shape specified by the input, filled with a constant value specified in an attribute
        "ConstantOfShape" => {
            if let InputTensor::I64(input_shape) = &inputs[0] {
                let element_count = input_shape.iter().product::<i64>() as usize;
                if let Ok(constant_value_tensor) =
                    node.get_attribute_value::<TensorProto>("value", None)
                {
                    match ScalarType::from_i32(constant_value_tensor.get_data_type())
                        .map_err(ConstantFoldingError::UnsupportedDataType)?
                    {
                        ScalarType::F32 => {
                            let fd = constant_value_tensor.get_float_data();
                            if fd.is_empty() {
                                return Err(ConstantFoldingError::InvalidNode(
                                    "value tensor for ConstantOfShape is empty".to_string(),
                                ));
                            }
                            Some(vec![OutputTensor::F32(vec![fd[0]; element_count])])
                        }
                        ScalarType::I64 => {
                            let fd = constant_value_tensor.get_int64_data();
                            if fd.is_empty() {
                                return Err(ConstantFoldingError::InvalidNode(
                                    "value tensor for ConstantOfShape is empty".to_string(),
                                ));
                            }
                            Some(vec![OutputTensor::I64(vec![fd[0]; element_count])])
                        }
                        ScalarType::I32 => {
                            let fd = constant_value_tensor.get_int32_data();
                            if fd.is_empty() {
                                return Err(ConstantFoldingError::InvalidNode(
                                    "value tensor for ConstantOfShape is empty".to_string(),
                                ));
                            }
                            Some(vec![OutputTensor::I32(vec![fd[0]; element_count])])
                        }
                        ScalarType::U8 => {
                            let fd = constant_value_tensor.get_raw_data();
                            if fd.is_empty() {
                                return Err(ConstantFoldingError::InvalidNode(
                                    "value tensor for ConstantOfShape is empty".to_string(),
                                ));
                            }
                            Some(vec![OutputTensor::U8(vec![fd[0]; element_count])])
                        }
                    }
                } else {
                    // The default value is a zero f32
                    Some(vec![OutputTensor::F32(vec![0.0; element_count])])
                }
            } else {
                return Err(ConstantFoldingError::InvalidNode(
                    "ConstantOfShape node input tensor has invalid type, should be i64".to_string(),
                ));
            }
        }

        _ => {
            // Try to run on GPU
            let mut graph = GraphProto::new();
            graph.set_input(RepeatedField::from(
                node.input
                    .iter()
                    .enumerate()
                    .map(|(index, input)| {
                        let shape = &shapes[input];
                        input_to_value_info(shape, &format!("input_{}", index))
                    })
                    .collect::<Vec<_>>(),
            ));

            graph.set_output(RepeatedField::from(
                node.output
                    .iter()
                    .enumerate()
                    .map(|(index, _output)| {
                        let shape = &output_shapes[index];
                        input_to_value_info(shape, &format!("output_{}", index))
                    })
                    .collect::<Vec<_>>(),
            ));

            let mut temp_node = node.clone();
            temp_node.set_output(RepeatedField::from(
                graph
                    .output
                    .iter()
                    .map(|otp| otp.get_name().to_string())
                    .collect::<Vec<String>>(),
            ));
            temp_node.set_input(RepeatedField::from(
                graph
                    .input
                    .iter()
                    .map(|otp| otp.get_name().to_string())
                    .collect::<Vec<String>>(),
            ));
            graph.set_node(RepeatedField::from(vec![temp_node]));

            let model = model_with_opset(graph, opset_version);

            let session = match Session::from_model(model).await {
                Ok(v) => v,
                Err(e) => {
                    if let SessionError::GpuError(GpuError::CompileError {
                        error: CompileError::UnimplementedOp(op_name),
                        ..
                    }) = e
                    {
                        log::info!("could not constant-fold node '{}', because op '{}' is not yet implemented", node.get_name(), op_name);
                        return Ok(None);
                    } else {
                        return Err(ConstantFoldingError::CalculationError(e));
                    }
                }
            };

            let mut named_inputs: HashMap<String, InputTensor> = HashMap::new();
            for (index, input) in inputs.iter().enumerate() {
                let input: InputTensor = input.to_owned();
                named_inputs.insert(format!("input_{}", index), input);
            }

            let mut output_values = session
                .run(&named_inputs)
                .await
                .map_err(ConstantFoldingError::CalculationError)?;

            let outputs: Vec<OutputTensor> = (0..node.output.len())
                .map(|output_index| {
                    let output_key = format!("output_{}", output_index);
                    output_values.remove(&output_key).unwrap()
                })
                .collect();

            Some(outputs)
        }
    })
}

fn input_to_value_info(shape: &Shape, name: &str) -> ValueInfoProto {
    let mut ttp = TypeProto_Tensor::new();
    ttp.set_elem_type(shape.data_type.to_datatype().value());
    let mut tsp = TensorShapeProto::new();
    tsp.set_dim(RepeatedField::from(
        shape
            .dims
            .iter()
            .map(|x| {
                let mut tdp = TensorShapeProto_Dimension::new();
                tdp.set_dim_value(*x as i64);
                tdp
            })
            .collect::<Vec<TensorShapeProto_Dimension>>(),
    ));
    ttp.set_shape(tsp);
    let mut ftp = TypeProto::new();
    ftp.set_tensor_type(ttp);
    let mut vip = ValueInfoProto::new();
    vip.set_name(name.to_string());
    vip.set_field_type(ftp);
    vip
}

fn calculate_shape_operator(
    node: &NodeProto,
    input_shape: &Shape,
) -> Result<OutputTensor, ConstantFoldingError> {
    let input_dims: Vec<i64> = input_shape.dims.iter().map(|x| *x as i64).collect();
    let mut start = node.get_attribute_value("start", Some(0)).unwrap();
    let mut end = node
        .get_attribute_value("start", Some(input_dims.len() as i64 - 1))
        .unwrap();
    if start < 0 {
        start += input_dims.len() as i64;
    }
    if end < 0 {
        end += input_dims.len() as i64;
    }
    if start > end {
        return Err(ConstantFoldingError::InvalidNode(format!(
            "end attribute value ({}) for Shape node should be higher than start attribute ({})",
            end, start
        )));
    }

    let output_shape: Vec<i64> = (input_dims[(start as usize)..=(end as usize)]).into();
    if output_shape.is_empty() {
        log::warn!("Shape operator results in an empty output shape which is probably an issue... start={start} end={end} input_shape={}", input_shape);
    }

    Ok(OutputTensor::I64(output_shape))
}
