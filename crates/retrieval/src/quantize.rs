pub fn quantize(vector: &[f32]) -> Option<(Vec<u8>, f32)> {
    if vector.is_empty() || vector.len()>2048 || vector.iter().any(|v|!v.is_finite()) { return None; }
    let maximum=vector.iter().map(|v|v.abs()).fold(0.0,f32::max);
    let scale=maximum/127.0;
    if !scale.is_finite() || scale<=0.0 { return None; }
    Some((vector.iter().map(|value| (value/scale).round().clamp(-127.0,127.0) as i8 as u8).collect(),scale))
}

pub fn dequantize(bytes: &[u8], scale: f32) -> Option<Vec<f32>> {
    if bytes.is_empty() || bytes.len()>2048 || !scale.is_finite() || scale<=0.0 {return None;}
    let mut values:Vec<_>=bytes.iter().map(|value|f32::from(*value as i8)*scale).collect();
    let norm=values.iter().map(|v|f64::from(*v).powi(2)).sum::<f64>().sqrt();
    if !norm.is_finite() || norm<=0.0 {return None;}
    for value in &mut values {*value=(f64::from(*value)/norm) as f32;}
    Some(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compressed_vectors_preserve_direction_and_refuse_invalid_values() {
        for dimensions in [2,512,768,1024,2048] {
            let input:Vec<_>=(0..dimensions).map(|n|((n as f32+1.0)*0.731).sin()).collect();
            let norm=input.iter().map(|v|v*v).sum::<f32>().sqrt();
            let (bytes,scale)=quantize(&input).unwrap();
            let restored=dequantize(&bytes,scale).unwrap();
            let cosine=input.iter().zip(&restored).map(|(a,b)|a/norm*b).sum::<f32>();
            assert!(cosine>0.9999,"{dimensions}: {cosine}");
            assert_eq!(bytes.len(),dimensions);
        }
        assert!(quantize(&[f32::NAN]).is_none());
        assert!(quantize(&[0.0]).is_none());
        assert!(dequantize(&[0],1.0).is_none());
        assert!(dequantize(&[1],f32::INFINITY).is_none());
    }
}
